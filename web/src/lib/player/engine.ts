import type { Id, PlaybackState } from '../contracts';

// 播放引擎是唯一的音频状态来源：组件不各自维护 HTMLAudioElement。
// 位置与时长一律用整数毫秒；浏览器通过 GET/Range 拉取 /media/tracks/{id}。

export interface PlayerSnapshot {
  state: PlaybackState;
  trackId: Id | null;
  positionMs: number;
  durationMs: number | null;
}

export function mediaSource(trackId: Id): string {
  return `/media/tracks/${encodeURIComponent(trackId)}`;
}

type Listener = (snapshot: PlayerSnapshot) => void;

export class PlayerEngine {
  private readonly audio: HTMLAudioElement;
  private readonly listeners = new Set<Listener>();
  private trackId: Id | null = null;
  private state: PlaybackState = 'idle';

  constructor(audio: HTMLAudioElement = new Audio()) {
    this.audio = audio;
    audio.addEventListener('loadstart', () => this.set('loading'));
    audio.addEventListener('waiting', () => this.set('buffering'));
    audio.addEventListener('canplay', () =>
      this.set(this.audio.paused ? 'paused' : 'playing'),
    );
    audio.addEventListener('playing', () => this.set('playing'));
    audio.addEventListener('pause', () =>
      this.set(this.audio.ended ? 'idle' : 'paused'),
    );
    audio.addEventListener('ended', () => this.set('idle'));
    audio.addEventListener('error', () => this.set('error'));
    audio.addEventListener('timeupdate', () => this.emit());
  }

  get snapshot(): PlayerSnapshot {
    return {
      state: this.state,
      trackId: this.trackId,
      positionMs: Math.round((this.audio.currentTime || 0) * 1000),
      durationMs: Number.isFinite(this.audio.duration)
        ? Math.round(this.audio.duration * 1000)
        : null,
    };
  }

  subscribe(listener: Listener): () => void {
    this.listeners.add(listener);
    listener(this.snapshot);
    return () => this.listeners.delete(listener);
  }

  async play(trackId: Id): Promise<void> {
    if (this.trackId !== trackId) {
      this.trackId = trackId;
      this.audio.src = mediaSource(trackId);
      this.set('loading');
    }
    await this.audio.play();
  }

  pause(): void {
    this.audio.pause();
  }

  // 拖动：直接写 currentTime，浏览器随后用 Range 请求补齐字节。
  seekMs(positionMs: number): void {
    if (this.audio.duration > 0) {
      this.audio.currentTime = Math.min(positionMs / 1000, this.audio.duration);
    }
    this.emit();
  }

  destroy(): void {
    this.audio.pause();
    this.audio.removeAttribute('src');
    this.audio.load();
    this.listeners.clear();
  }

  private set(state: PlaybackState): void {
    this.state = state;
    this.emit();
  }

  private emit(): void {
    const snapshot = this.snapshot;
    for (const listener of this.listeners) listener(snapshot);
  }
}
