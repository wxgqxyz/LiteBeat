import { ApiError, searchTracks } from './api';
import type { TrackSummary } from './contracts';

// 搜索的四道防线都在这里，SearchBox 只负责把输入法事件转成命令：
// 合成期间不搜、防抖、AbortController 取消在途请求、按序号丢弃后到的旧响应。

export const MAX_QUERY_CHARS = 64;
export const DEFAULT_DEBOUNCE_MS = 250;

export interface SearchPage {
  items: TrackSummary[];
  next_cursor: string | null;
  has_more: boolean;
}

export type SearchPhase = 'blank' | 'too-long' | 'composing' | 'idle' | 'loading' | 'ready' | 'empty' | 'error';

export interface SearchState {
  text: string;
  query: string;
  phase: SearchPhase;
  items: TrackSummary[];
  next_cursor: string | null;
  has_more: boolean;
  message: string | null;
  loadingMore: boolean;
  dropped: number;
}

export type SearchFetcher = (
  query: string,
  options: { cursor?: string | null; limit: number; signal: AbortSignal },
) => Promise<SearchPage>;

export interface SearchControllerOptions {
  fetcher?: SearchFetcher;
  debounceMs?: number;
  limit?: number;
  maxChars?: number;
}

function initialState(): SearchState {
  return {
    text: '',
    query: '',
    phase: 'blank',
    items: [],
    next_cursor: null,
    has_more: false,
    message: null,
    loadingMore: false,
    dropped: 0,
  };
}

const DEFAULT_FETCHER: SearchFetcher = (query, options) =>
  searchTracks(query, { limit: options.limit, cursor: options.cursor, signal: options.signal });

export class SearchController {
  private readonly fetcher: SearchFetcher;
  private readonly debounceMs: number;
  private readonly limit: number;
  private readonly maxChars: number;
  private state: SearchState = initialState();
  private readonly listeners = new Set<(state: SearchState) => void>();
  private composing = false;
  private timer: ReturnType<typeof setTimeout> | null = null;
  // 每次真正发请求自增；响应回来时序号不是最新的即为过期响应，直接丢弃。
  private generation = 0;
  private inFlight: AbortController | null = null;

  constructor(options: SearchControllerOptions = {}) {
    this.fetcher = options.fetcher ?? DEFAULT_FETCHER;
    this.debounceMs = options.debounceMs ?? DEFAULT_DEBOUNCE_MS;
    this.limit = options.limit ?? 20;
    this.maxChars = options.maxChars ?? MAX_QUERY_CHARS;
  }

  get snapshot(): SearchState {
    return this.state;
  }

  subscribe(listener: (state: SearchState) => void): () => void {
    this.listeners.add(listener);
    listener(this.state);
    return () => this.listeners.delete(listener);
  }

  setComposing(composing: boolean): void {
    this.composing = composing;
    if (composing) {
      this.cancelPending();
      this.patch({ phase: 'composing' });
      return;
    }
    this.schedule();
  }

  setText(text: string): void {
    this.patch({ text });
    if (this.composing) return;
    this.schedule();
  }

  // 回车立即提交，跳过防抖。
  submit(): void {
    this.cancelPending();
    this.run();
  }

  cancel(): void {
    this.cancelPending();
    this.inFlight?.abort();
    this.inFlight = null;
  }

  reset(): void {
    this.cancel();
    this.generation += 1;
    this.state = initialState();
    this.emit();
  }

  async loadMore(): Promise<void> {
    if (!this.state.has_more || this.state.loadingMore || !this.state.query) return;
    const generation = this.generation;
    const cursor = this.state.next_cursor;
    this.patch({ loadingMore: true, message: null });
    const controller = new AbortController();
    try {
      const page = await this.fetcher(this.state.query, {
        cursor,
        limit: this.limit,
        signal: controller.signal,
      });
      if (generation !== this.generation) {
        this.patch({ loadingMore: false, dropped: this.state.dropped + 1 });
        return;
      }
      this.patch({
        loadingMore: false,
        items: [...this.state.items, ...page.items],
        next_cursor: page.next_cursor,
        has_more: page.has_more,
        phase: 'ready',
      });
    } catch (caught) {
      if (controller.signal.aborted || generation !== this.generation) return;
      this.patch({ loadingMore: false, phase: 'error', message: describe(caught) });
    }
  }

  private cancelPending(): void {
    if (this.timer !== null) {
      clearTimeout(this.timer);
      this.timer = null;
    }
  }

  private schedule(): void {
    this.cancelPending();
    const trimmed = this.state.text.trim();
    if (trimmed.length === 0) {
      this.inFlight?.abort();
      this.inFlight = null;
      this.generation += 1;
      this.patch({ phase: 'blank', items: [], next_cursor: null, has_more: false, query: '', message: null });
      return;
    }
    if (trimmed.length > this.maxChars) {
      this.patch({ phase: 'too-long', message: `查询最多 ${this.maxChars} 个字符，请缩短关键词。` });
      return;
    }
    this.timer = setTimeout(() => {
      this.timer = null;
      this.run();
    }, this.debounceMs);
  }

  private run(): void {
    const query = this.state.text.trim();
    if (query.length === 0 || query.length > this.maxChars) return;
    if (this.composing) return;
    this.generation += 1;
    const generation = this.generation;
    this.inFlight?.abort();
    const controller = new AbortController();
    this.inFlight = controller;
    this.patch({
      query,
      phase: 'loading',
      items: [],
      next_cursor: null,
      has_more: false,
      message: null,
    });
    void this.fetchPage(query, controller, generation);
  }

  private async fetchPage(
    query: string,
    controller: AbortController,
    generation: number,
  ): Promise<void> {
    try {
      const page = await this.fetcher(query, {
        cursor: null,
        limit: this.limit,
        signal: controller.signal,
      });
      if (generation !== this.generation) {
        // 后到的旧响应：界面已切到更新的查询，覆盖会把结果倒退回过期数据。
        this.patch({ dropped: this.state.dropped + 1 });
        return;
      }
      this.inFlight = null;
      this.patch({
        phase: page.items.length === 0 ? 'empty' : 'ready',
        items: page.items,
        next_cursor: page.next_cursor,
        has_more: page.has_more,
      });
    } catch (caught) {
      if (controller.signal.aborted || generation !== this.generation) {
        if (!controller.signal.aborted) this.patch({ dropped: this.state.dropped + 1 });
        return;
      }
      this.patch({ phase: 'error', message: describe(caught) });
    }
  }

  private patch(partial: Partial<SearchState>): void {
    this.state = { ...this.state, ...partial };
    this.emit();
  }

  private emit(): void {
    const state = this.state;
    for (const listener of this.listeners) listener(state);
  }
}

function describe(caught: unknown): string {
  if (caught instanceof ApiError) return `${caught.message}（${caught.code} · ${caught.requestId || '无请求号'}）`;
  return caught instanceof Error ? caught.message : '搜索失败，请稍后重试';
}
