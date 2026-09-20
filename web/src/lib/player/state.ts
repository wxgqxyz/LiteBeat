import { ApiError, clearSessionCache, getTrack } from '../api';
import type { Id, TrackSummary } from '../contracts';
import { PlayerEngine, type AudioLike, type PlayerSnapshot } from './engine';
import { PlaybackQueue, QUEUE_LIMIT, type QueueSnapshotData } from './queue';

// 播放器状态是全局单例：audio 元素由根布局 AppShell 挂一次，
// 路由切换只重渲染页面内容，引擎和队列都活在同一模块作用域里，播放才不会断。

export const PLAYER_STORAGE_KEY = 'litebeat.player.v1';
export const HYDRATE_BATCH = 50;

export interface StorageLike {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

export interface PersistedPlayer {
  version: 1;
  queue: QueueSnapshotData;
  positionMs: number;
  savedAt: number;
}

export interface QueueView {
  size: number;
  limit: number;
  current: number;
  shuffle: boolean;
  repeatOne: boolean;
  truncated: boolean;
  tracks: TrackSummary[];
  missing: number;
}

export interface StoreSnapshot {
  player: PlayerSnapshot;
  queue: QueueView;
  notice: string | null;
}

function browserStorage(): StorageLike | null {
  try {
    return typeof window === 'undefined' ? null : window.localStorage;
  } catch {
    return null;
  }
}

const POSITION_WRITE_INTERVAL_MS = 2000;

export class PlayerStore {
  readonly engine = new PlayerEngine();
  readonly queue = new PlaybackQueue();
  private readonly listeners = new Set<(snapshot: StoreSnapshot) => void>();
  private notice: string | null = null;
  private restoredPositionMs = 0;
  private lastPositionWrite = 0;
  private unSubscribeEngine: (() => void) | null = null;
  private unSubscribeEnded: (() => void) | null = null;

  constructor(private storage: StorageLike | null = browserStorage()) {
    this.wireEngine();
  }

  private wireEngine(): void {
    this.unSubscribeEngine?.();
    this.unSubscribeEnded?.();
    this.unSubscribeEngine = this.engine.subscribe((player) => {
      this.emit();
      // 真正开始走进度后就不再需要恢复位置。
      if (player.positionMs > 0 || player.state === 'playing') this.restoredPositionMs = 0;
      this.persistPosition(player);
    });
    this.unSubscribeEnded = this.engine.onEnded(() => void this.handleEnded());
  }

  useStorage(storage: StorageLike | null): void {
    this.storage = storage;
  }

  get snapshot(): StoreSnapshot {
    const player =
      this.restoredPositionMs > 0 && this.engine.snapshot.positionMs === 0
        ? { ...this.engine.snapshot, positionMs: this.restoredPositionMs }
        : this.engine.snapshot;
    return {
      player,
      queue: {
        size: this.queue.size,
        limit: this.queue.limit,
        current: this.queue.currentIndex,
        shuffle: this.queue.shuffle,
        repeatOne: this.queue.repeatOne,
        truncated: this.queue.truncated,
        tracks: this.queue.tracksInOrder,
        missing: this.queue.missingDetails().length,
      },
      notice: this.notice,
    };
  }

  subscribe(listener: (snapshot: StoreSnapshot) => void): () => void {
    this.listeners.add(listener);
    listener(this.snapshot);
    return () => this.listeners.delete(listener);
  }

  attachAudio(element: AudioLike | null): void {
    if (element) this.engine.attach(element);
    else this.engine.detach();
  }

  setNotice(notice: string | null): void {
    this.notice = notice;
    this.emit();
  }

  // 用户动作入口：先准备媒体再请求播放，浏览器拒绝时引擎会落到可解释状态。
  async play(track: TrackSummary): Promise<void> {
    let index = this.queue.indexOf(track.id);
    if (index < 0) {
      if (!this.queue.add(track)) this.warnTruncated();
      index = this.queue.indexOf(track.id);
    }
    this.queue.setCursorIndex(index);
    await this.engine.load(track);
    await this.engine.play();
    this.persist();
  }

  async playAt(index: number): Promise<void> {
    const track = this.queue.trackAt(index);
    if (!track) return;
    this.queue.setCursorIndex(index);
    await this.engine.load(track);
    await this.engine.play();
    this.persist();
  }

  async toggle(): Promise<void> {
    const { state, track } = this.engine.snapshot;
    if (state === 'playing' || state === 'buffering') {
      this.engine.pause();
      return;
    }
    if (track) await this.engine.play();
  }

  pause(): void {
    this.engine.pause();
  }

  seek(seconds: number): void {
    this.restoredPositionMs = 0;
    this.engine.seek(seconds);
  }

  async enqueue(track: TrackSummary): Promise<void> {
    if (!this.queue.add(track)) this.warnTruncated();
    this.persist();
  }

  async enqueueNext(track: TrackSummary): Promise<void> {
    if (!this.queue.playNext(track)) this.warnTruncated();
    this.persist();
  }

  // 「播放全部」走 queue-ids：只拿 ID，最多 1000 个，truncated 必须如实提示。
  async enqueueIds(ids: Id[], truncated: boolean): Promise<void> {
    const accepted = this.queue.addIds(ids);
    if (accepted && truncated) {
      this.setNotice(`队列上限 ${QUEUE_LIMIT} 首：后端返回的 ID 已被截断。`);
    } else if (!accepted) {
      this.warnTruncated();
    } else {
      this.setNotice(null);
    }
    this.persist();
  }

  removeAt(index: number): void {
    this.queue.removeAt(index);
    this.persist();
  }

  clearQueue(): void {
    this.queue.clear();
    this.persist();
  }

  toggleShuffle(): void {
    this.queue.toggleShuffle();
    this.persist();
  }

  toggleRepeatOne(): void {
    this.queue.toggleRepeatOne();
    this.persist();
  }

  async skipNext(): Promise<void> {
    if (!this.queue.next()) {
      this.setNotice('队列已到末尾。');
      return;
    }
    await this.playAt(this.queue.currentIndex);
  }

  async skipPrevious(): Promise<void> {
    if (!this.queue.previous()) return;
    await this.playAt(this.queue.currentIndex);
  }

  // 队列里只有 ID 时按需补详情；一次最多 HYDRATE_BATCH 首，避免打满只读预算。
  async hydratePending(limit = HYDRATE_BATCH): Promise<number> {
    const pending = this.queue.missingDetails().slice(0, Math.max(0, limit));
    if (pending.length === 0) return 0;
    const loaded: TrackSummary[] = [];
    for (const id of pending) {
      try {
        loaded.push(await getTrack(id));
      } catch (error) {
        if (!(error instanceof ApiError)) throw error;
        // 单条详情失败不阻断整批：保留占位条目，界面上按 ID 显示。
      }
    }
    this.queue.applyDetails(loaded);
    this.persist();
    return loaded.length;
  }

  persist(): void {
    if (!this.storage) return;
    const payload: PersistedPlayer = {
      version: 1,
      queue: this.queue.toData(),
      positionMs: this.engine.snapshot.positionMs,
      savedAt: Date.now(),
    };
    try {
      this.storage.setItem(PLAYER_STORAGE_KEY, JSON.stringify(payload));
    } catch {
      // 隐私模式或配额写满：持久化失败不能影响播放。
    }
  }

  private async handleEnded(): Promise<void> {
    // 单曲循环：同一首回到起点重播，不往队列里复制条目。
    if (this.queue.repeatOne) {
      this.engine.seek(0);
      await this.engine.play();
      return;
    }
    if (this.queue.next()) await this.playAt(this.queue.currentIndex);
    else this.persist();
  }

  private persistPosition(player: PlayerSnapshot): void {
    if (player.state !== 'playing' && player.state !== 'paused') return;
    const now = Date.now();
    // timeupdate 每秒触发多次，写盘节流到 2 秒一次。
    if (player.state === 'playing' && now - this.lastPositionWrite < POSITION_WRITE_INTERVAL_MS) {
      return;
    }
    this.lastPositionWrite = now;
    this.persist();
  }

  async restore(): Promise<boolean> {
    if (!this.storage) return false;
    let raw: string | null = null;
    try {
      raw = this.storage.getItem(PLAYER_STORAGE_KEY);
    } catch {
      return false;
    }
    if (!raw) return false;
    let payload: PersistedPlayer | null = null;
    try {
      payload = JSON.parse(raw) as PersistedPlayer;
    } catch {
      return false;
    }
    if (!payload || payload.version !== 1 || !Array.isArray(payload.queue?.ids)) return false;
    this.queue.restore(payload.queue);
    const track = this.queue.currentTrack;
    if (!track) {
      this.emit();
      return true;
    }
    // 只恢复「选定 + 位置」，是否播放由用户点按钮决定。
    await this.engine.load(track);
    if (payload.positionMs > 0) {
      this.restoredPositionMs = payload.positionMs;
      this.engine.seek(payload.positionMs / 1000);
    }
    this.emit();
    return true;
  }

  forget(): void {
    this.engine.dispose();
    this.queue.clear();
    this.restoredPositionMs = 0;
    try {
      this.storage?.removeItem(PLAYER_STORAGE_KEY);
    } catch {
      // 忽略：存储不可用时内存状态已经清干净。
    }
    this.emit();
  }

  async signOut(): Promise<void> {
    this.forget();
    clearSessionCache();
  }

  private warnTruncated(): void {
    this.setNotice(`队列最多 ${this.queue.limit} 首，超出部分已丢弃。`);
  }

  private emit(): void {
    const snapshot = this.snapshot;
    for (const listener of this.listeners) listener(snapshot);
  }
}

export const player = new PlayerStore();
