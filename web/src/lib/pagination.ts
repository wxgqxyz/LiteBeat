import { ApiError } from './api';
import type { Page } from './contracts';

// 曲库/专辑/专辑详情共用一套游标分页：一次只取一页，靠 next_cursor 继续翻页；
// 游标失效（INVALID_CURSOR）时丢弃本地游标重拉首页，绝不带着坏游标循环重试。

export type ListPhase = 'loading' | 'ready' | 'empty' | 'error';

export interface ListState<T> {
  phase: ListPhase;
  items: T[];
  next_cursor: string | null;
  has_more: boolean;
  loadingMore: boolean;
  message: string | null;
  code: string | null;
  requestId: string | null;
}

export interface PaginatorOptions<T> {
  fetch: (cursor: string | null, signal: AbortSignal) => Promise<Page<T>>;
}

function emptyState<T>(): ListState<T> {
  return {
    phase: 'loading',
    items: [],
    next_cursor: null,
    has_more: false,
    loadingMore: false,
    message: null,
    code: null,
    requestId: null,
  };
}

function isCursorError(caught: unknown): boolean {
  return caught instanceof ApiError && caught.code === 'INVALID_CURSOR';
}

export class Paginator<T> {
  private state: ListState<T> = emptyState();
  private readonly listeners = new Set<(state: ListState<T>) => void>();
  private inFlight: AbortController | null = null;
  // 同一次加载里因游标失效重拉首页的次数上限为 1。
  private cursorRetries = 0;

  constructor(private readonly options: PaginatorOptions<T>) {}

  get snapshot(): ListState<T> {
    return this.state;
  }

  subscribe(listener: (state: ListState<T>) => void): () => void {
    this.listeners.add(listener);
    listener(this.state);
    return () => this.listeners.delete(listener);
  }

  patch(partial: Partial<ListState<T>>): void {
    this.state = { ...this.state, ...partial };
    this.emit();
  }

  async load(): Promise<void> {
    this.inFlight?.abort();
    const controller = new AbortController();
    this.inFlight = controller;
    this.cursorRetries = 0;
    this.state = { ...emptyState<T>() };
    this.emit();
    await this.fetchPage(null, controller, true);
  }

  async loadMore(): Promise<void> {
    if (!this.state.has_more || this.state.loadingMore || !this.state.next_cursor) return;
    const controller = new AbortController();
    this.patch({ loadingMore: true });
    await this.fetchPage(this.state.next_cursor, controller, false);
  }

  stop(): void {
    this.inFlight?.abort();
    this.inFlight = null;
  }

  private async fetchPage(
    cursor: string | null,
    controller: AbortController,
    first: boolean,
  ): Promise<void> {
    try {
      const page = await this.options.fetch(cursor, controller.signal);
      const items = first ? page.items : [...this.state.items, ...page.items];
      this.patch({
        loadingMore: false,
        phase: items.length === 0 ? 'empty' : 'ready',
        items,
        next_cursor: page.next_cursor,
        has_more: page.has_more,
        message: null,
        code: null,
        requestId: null,
      });
    } catch (caught) {
      if (controller.signal.aborted) return;
      if (isCursorError(caught) && this.cursorRetries === 0) {
        this.cursorRetries += 1;
        const retry = new AbortController();
        this.inFlight = retry;
        await this.fetchPage(null, retry, true);
        return;
      }
      const error = caught instanceof ApiError ? caught : null;
      this.patch({
        loadingMore: false,
        phase: 'error',
        message: error ? error.message : '列表加载失败，请稍后重试',
        code: error?.code ?? 'UNKNOWN',
        requestId: error?.requestId ?? null,
      });
    }
  }

  private emit(): void {
    const state = this.state;
    for (const listener of this.listeners) listener(state);
  }
}
