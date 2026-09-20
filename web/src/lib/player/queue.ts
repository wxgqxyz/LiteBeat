import type { Id, TrackSummary } from '../contracts';

// 队列只保存 ID 顺序 + 已知详情：后端 queue-ids 端点上限 1000 个 ID 且不回详情，
// 随机播放因此只能打乱 ID 序列，单曲循环也只改播放模式、绝不往队列里复制条目。

export const QUEUE_LIMIT = 1000;

export interface QueueShuffle {
  shuffle: boolean;
  repeatOne: boolean;
}

export interface QueueSnapshotData {
  version: 1;
  ids: Id[];
  tracks: TrackSummary[];
  current: number;
  shuffle: boolean;
  repeatOne: boolean;
  truncated: boolean;
}

export interface PlaybackQueueOptions {
  limit?: number;
  random?: () => number;
}

// 详情未知的占位条目：标题留空由界面按 ID 兜底显示。
export function placeholderTrack(id: Id): TrackSummary {
  return { id, title: '', artist: '', album_id: null, duration_ms: null, available: true };
}

function shuffleIds(ids: Id[], random: () => number): Id[] {
  const result = [...ids];
  for (let index = result.length - 1; index > 0; index -= 1) {
    const swap = Math.floor(random() * (index + 1));
    [result[index], result[swap]] = [result[swap], result[index]];
  }
  return result;
}

export class PlaybackQueue {
  readonly limit: number;
  private readonly random: () => number;
  private ids: Id[] = [];
  private readonly tracks = new Map<Id, TrackSummary>();
  private cursor = -1;
  private flags: QueueShuffle = { shuffle: false, repeatOne: false };
  private hitLimit = false;

  constructor(options: PlaybackQueueOptions = {}) {
    this.limit = Math.min(options.limit ?? QUEUE_LIMIT, QUEUE_LIMIT);
    this.random = options.random ?? Math.random;
  }

  get size(): number {
    return this.ids.length;
  }

  get currentIndex(): number {
    return this.cursor;
  }

  get truncated(): boolean {
    return this.hitLimit;
  }

  get shuffle(): boolean {
    return this.flags.shuffle;
  }

  get repeatOne(): boolean {
    return this.flags.repeatOne;
  }

  get order(): readonly Id[] {
    return this.ids;
  }

  get tracksInOrder(): TrackSummary[] {
    return this.ids.map((id) => this.tracks.get(id) ?? placeholderTrack(id));
  }

  get currentTrack(): TrackSummary | null {
    return this.trackAt(this.cursor);
  }

  trackAt(index: number): TrackSummary | null {
    const id = this.ids[index];
    return id === undefined ? null : (this.tracks.get(id) ?? placeholderTrack(id));
  }

  has(id: Id): boolean {
    return this.ids.includes(id);
  }

  // 返回 true 表示完整加入；false 表示因上限被截断。
  add(tracks: TrackSummary | TrackSummary[]): boolean {
    const incoming = Array.isArray(tracks) ? tracks : [tracks];
    let acceptedAll = true;
    for (const track of incoming) {
      if (this.ids.length >= this.limit) {
        this.hitLimit = true;
        acceptedAll = false;
        continue;
      }
      this.tracks.set(track.id, track);
      this.ids.push(track.id);
    }
    return acceptedAll;
  }

  // 详情未加载的 ID 也能入队：后端按 ID 直接取流，标题等界面占位显示。
  addIds(ids: Id[]): boolean {
    let acceptedAll = true;
    for (const id of ids) {
      if (this.ids.length >= this.limit) {
        this.hitLimit = true;
        acceptedAll = false;
        continue;
      }
      if (!this.tracks.has(id)) this.tracks.set(id, placeholderTrack(id));
      this.ids.push(id);
    }
    return acceptedAll;
  }

  // 「下一首播放」：插在当前曲目之后，不影响正在播的条目。
  playNext(track: TrackSummary): boolean {
    this.tracks.set(track.id, track);
    if (this.ids.length >= this.limit) {
      this.hitLimit = true;
      return false;
    }
    this.ids.splice(this.cursor < 0 ? 0 : this.cursor + 1, 0, track.id);
    return true;
  }

  removeAt(index: number): TrackSummary | null {
    const [removed] = this.ids.splice(index, 1);
    if (removed === undefined) return null;
    const track = this.tracks.get(removed) ?? null;
    if (!this.ids.includes(removed)) this.tracks.delete(removed);
    if (index < this.cursor) this.cursor -= 1;
    else if (index === this.cursor) this.cursor = Math.min(this.cursor, this.ids.length - 1);
    return track;
  }

  remove(id: Id): number {
    let removed = 0;
    for (let index = this.ids.length - 1; index >= 0; index -= 1) {
      if (this.ids[index] === id) {
        this.removeAt(index);
        removed += 1;
      }
    }
    return removed;
  }

  clear(): void {
    this.ids = [];
    this.tracks.clear();
    this.cursor = -1;
    this.hitLimit = false;
  }

  // 随机播放只打乱「尚未播放」部分的 ID 顺序。
  toggleShuffle(): boolean {
    this.flags.shuffle = !this.flags.shuffle;
    if (this.flags.shuffle) {
      const head = this.ids.slice(0, Math.max(this.cursor + 1, 0));
      const tail = shuffleIds(this.ids.slice(head.length), this.random);
      this.ids = [...head, ...tail];
    }
    return this.flags.shuffle;
  }

  // 单曲循环不改队列长度：只记模式，next() 仍指向同一条目。
  toggleRepeatOne(): boolean {
    this.flags.repeatOne = !this.flags.repeatOne;
    return this.flags.repeatOne;
  }

  indexOf(id: Id): number {
    return this.ids.indexOf(id);
  }

  setCursorIndex(index: number): number {
    this.cursor = index >= 0 && index < this.ids.length ? index : -1;
    return this.cursor;
  }

  // 返回下一首要播放的曲目；null 表示队列到此为止。
  next(): TrackSummary | null {
    if (this.flags.repeatOne) return this.currentTrack;
    if (this.cursor + 1 >= this.ids.length) return null;
    this.cursor += 1;
    return this.currentTrack;
  }

  previous(): TrackSummary | null {
    if (this.cursor <= 0) return null;
    this.cursor -= 1;
    return this.currentTrack;
  }

  applyDetails(tracks: TrackSummary[]): void {
    for (const track of tracks) {
      if (this.tracks.has(track.id)) this.tracks.set(track.id, track);
    }
  }

  missingDetails(): Id[] {
    return this.ids.filter((id) => (this.tracks.get(id)?.title ?? '') === '');
  }

  toData(): QueueSnapshotData {
    return {
      version: 1,
      ids: [...this.ids],
      tracks: this.tracksInOrder,
      current: this.cursor,
      shuffle: this.flags.shuffle,
      repeatOne: this.flags.repeatOne,
      truncated: this.hitLimit,
    };
  }

  restore(data: QueueSnapshotData): void {
    if (data.version !== 1 || !Array.isArray(data.ids)) return;
    this.clear();
    const details = new Map(data.tracks.map((track) => [track.id, track]));
    for (const id of data.ids.slice(0, this.limit)) {
      this.tracks.set(id, details.get(id) ?? placeholderTrack(id));
      this.ids.push(id);
    }
    this.cursor = data.current >= 0 && data.current < this.ids.length ? data.current : -1;
    this.flags = { shuffle: data.shuffle === true, repeatOne: data.repeatOne === true };
    this.hitLimit = data.truncated === true || data.ids.length > this.limit;
  }
}
