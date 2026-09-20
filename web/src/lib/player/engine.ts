import type { Id, PlaybackState, TrackSummary } from '../contracts';

// 播放引擎是唯一的音频状态来源：组件不各自持有 HTMLAudioElement。
// 引擎本身是模块级单例（见 player/state.ts），audio 元素只在根布局渲染一次，
// 路由切换通过 attach/detach 复用同一个元素，绝不能重建，否则播放会中断。

export function mediaSource(trackId: Id): string {
  return `/media/tracks/${encodeURIComponent(trackId)}`;
}

// 只声明引擎真正用到的成员，单元测试据此注入假元素。
export interface AudioLike {
  src: string;
  currentTime: number;
  duration: number;
  paused: boolean;
  ended: boolean;
  readyState: number;
  play(): Promise<void>;
  pause(): void;
  load(): void;
  removeAttribute(name: string): void;
  addEventListener(type: string, listener: () => void): void;
  removeEventListener(type: string, listener: () => void): void;
}

export interface PlayerSnapshot {
  state: PlaybackState;
  track: TrackSummary | null;
  positionMs: number;
  durationMs: number | null;
  // 面向用户的解释文本：播放被拒绝或媒体出错时说明原因，不引入额外状态枚举。
  message: string | null;
}

type Listener = (snapshot: PlayerSnapshot) => void;

const TRACK_EVENTS = [
  'loadstart',
  'loadedmetadata',
  'loadeddata',
  'canplay',
  'playing',
  'waiting',
  'pause',
  'ended',
  'error',
  'timeupdate',
] as const;

export class PlayerEngine {
  private audio: AudioLike | null = null;
  private readonly handlers = new Map<string, () => void>();
  private readonly listeners = new Set<Listener>();
  private readonly endedHandlers = new Set<() => void>();
  private track: TrackSummary | null = null;
  private state: PlaybackState = 'idle';
  private message: string | null = null;
  // 元数据未到点时的 seek 意图，等 loadedmetadata 再落位，否则会静默丢失。
  private pendingSeekSeconds: number | null = null;

  attach(audio: AudioLike): void {
    if (this.audio === audio) return;
    this.detach();
    this.audio = audio;
    for (const type of TRACK_EVENTS) {
      const handler = () => this.onMediaEvent(type);
      this.handlers.set(type, handler);
      audio.addEventListener(type, handler);
    }
  }

  detach(): void {
    if (this.audio) {
      for (const [type, handler] of this.handlers) this.audio.removeEventListener(type, handler);
    }
    this.handlers.clear();
    this.audio = null;
  }

  get snapshot(): PlayerSnapshot {
    const audio = this.audio;
    if (!this.track) {
      return {
        state: this.state,
        track: null,
        positionMs: 0,
        durationMs: null,
        message: this.message,
      };
    }
    const seconds = audio && Number.isFinite(audio.currentTime) ? audio.currentTime : 0;
    return {
      state: this.state,
      track: this.track,
      positionMs: Math.round(seconds * 1000),
      durationMs: audio ? this.durationMs(audio) : (this.track.duration_ms ?? null),
      message: this.message,
    };
  }

  subscribe(listener: Listener): () => void {
    this.listeners.add(listener);
    listener(this.snapshot);
    return () => this.listeners.delete(listener);
  }

  // 自然播放结束的通知走独立通道：队列推进由 player/state.ts 决定。
  onEnded(handler: () => void): () => void {
    this.endedHandlers.add(handler);
    return () => this.endedHandlers.delete(handler);
  }

  // 只选定媒体并进入准备状态；是否发声由用户动作触发的 play() 决定。
  async load(track: TrackSummary): Promise<void> {
    this.track = track;
    this.message = null;
    this.pendingSeekSeconds = null;
    const audio = this.audio;
    if (!audio) {
      this.set('error', '音频元素尚未挂载，请重试');
      return;
    }
    if (!track.available) {
      audio.pause();
      audio.removeAttribute('src');
      audio.load();
      this.set('error', '曲目不可用：文件缺失或已被标记为不可播放');
      return;
    }
    this.set('loading', null);
    audio.src = mediaSource(track.id);
    audio.load();
  }

  // 浏览器可能拒绝播放：落到可解释的状态与文案，绝不留下未捕获的 rejection。
  async play(): Promise<void> {
    const audio = this.audio;
    if (!audio) {
      this.set('error', '音频元素尚未挂载，请重试');
      return;
    }
    if (!this.track) {
      this.set('error', '尚未选择曲目');
      return;
    }
    if (!this.track.available) {
      this.set('error', '曲目不可用：文件缺失或已被标记为不可播放');
      return;
    }
    try {
      await audio.play();
      this.set('playing', null);
    } catch (caught) {
      const rejection = describeRejection(caught);
      if (rejection.kind === 'autoplay') {
        this.set('paused', rejection.message);
      } else if (rejection.kind === 'interrupted') {
        this.set('loading', rejection.message);
      } else {
        this.set('error', rejection.message);
      }
    }
  }

  pause(): void {
    this.audio?.pause();
    if (this.state === 'playing' || this.state === 'buffering' || this.state === 'loading') {
      this.set('paused', this.message);
    }
  }

  // 秒为对外单位（与 <input type=range> 一致）；内部快照仍换算成整数毫秒。
  seek(seconds: number): void {
    const audio = this.audio;
    if (!audio || !Number.isFinite(seconds)) return;
    const target = Math.max(0, seconds);
    const duration = Number.isFinite(audio.duration) && audio.duration > 0 ? audio.duration : null;
    const clamped = duration === null ? target : Math.min(target, duration);
    if (audio.readyState >= 1) {
      audio.currentTime = clamped;
      this.pendingSeekSeconds = null;
    } else {
      this.pendingSeekSeconds = clamped;
    }
    this.emit();
  }

  // 释放媒体资源但保留订阅者与已挂载元素：退出登录后仍要在同一单例上重新播放。
  dispose(): void {
    const audio = this.audio;
    if (audio) {
      audio.pause();
      audio.removeAttribute('src');
      audio.load();
    }
    this.track = null;
    this.pendingSeekSeconds = null;
    this.set('idle', null);
  }

  private durationMs(audio: AudioLike): number | null {
    if (Number.isFinite(audio.duration) && audio.duration > 0) return Math.round(audio.duration * 1000);
    return this.track?.duration_ms != null ? this.track.duration_ms : null;
  }

  private onMediaEvent(type: string): void {
    // dispose 之后浏览器仍会补投事件：没有选定曲目时一律忽略，避免状态被拉回 loading。
    if (!this.track) return;
    const audio = this.audio;
    if (!audio) return;
    switch (type) {
      case 'loadstart':
        this.set('loading', this.message);
        break;
      case 'loadedmetadata':
        if (this.pendingSeekSeconds !== null) {
          audio.currentTime = this.pendingSeekSeconds;
          this.pendingSeekSeconds = null;
        }
        break;
      case 'loadeddata':
        this.set(this.state === 'playing' ? 'playing' : 'buffering', this.message);
        break;
      case 'canplay':
        if (this.state !== 'playing') this.set(audio.paused ? 'paused' : 'playing', this.message);
        break;
      case 'playing':
        this.set('playing', null);
        break;
      case 'waiting':
        this.set('buffering', this.message);
        break;
      case 'pause':
        if (!audio.ended) this.set('paused', this.message);
        break;
      case 'ended':
        this.set('idle', this.message);
        for (const handler of this.endedHandlers) handler();
        break;
      case 'error':
        this.set('error', '媒体加载失败：编码不支持或文件已损坏');
        break;
      case 'timeupdate':
        this.emit();
        break;
    }
  }

  private set(state: PlaybackState, message?: string | null): void {
    this.state = state;
    if (message !== undefined) this.message = message;
    this.emit();
  }

  private emit(): void {
    const snapshot = this.snapshot;
    for (const listener of this.listeners) listener(snapshot);
  }
}

function describeRejection(
  caught: unknown,
): { kind: 'autoplay' | 'interrupted' | 'failed'; message: string } {
  const name = caught instanceof Error ? caught.name : '';
  if (name === 'NotAllowedError') {
    return { kind: 'autoplay', message: '浏览器拒绝了自动播放，请点击播放按钮' };
  }
  if (name === 'AbortError') {
    return { kind: 'interrupted', message: '播放请求被打断（切歌过快），正在重新准备' };
  }
  const detail = caught instanceof Error && caught.message ? caught.message : '未知原因';
  return { kind: 'failed', message: `无法开始播放：${detail}` };
}
