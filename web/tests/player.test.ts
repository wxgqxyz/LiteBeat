import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ApiError } from '../src/lib/api';
import type { TrackSummary } from '../src/lib/contracts';
import { PlayerEngine, type AudioLike } from '../src/lib/player/engine';
import { PlaybackQueue, QUEUE_LIMIT, placeholderTrack } from '../src/lib/player/queue';
import { PlayerStore, type StorageLike } from '../src/lib/player/state';
import { SearchController, type SearchPage } from '../src/lib/search';
import { Paginator } from '../src/lib/pagination';

// 只为断言「没有未捕获 rejection」而需要 Node 钩子；用窄类型取用，避免为此引入 @types/node。
const nodeProcess = (
  globalThis as unknown as {
    process: {
      on(kind: 'unhandledRejection', handler: (reason: unknown) => void): void;
      off(kind: 'unhandledRejection', handler: (reason: unknown) => void): void;
    };
  }
).process;

function track(id: string, over: Partial<TrackSummary> = {}): TrackSummary {
  return {
    id,
    title: `曲目 ${id}`,
    artist: '歌手',
    album_id: '1',
    duration_ms: 240_000,
    available: true,
    ...over,
  };
}

class FakeAudio implements AudioLike {
  src = '';
  currentTime = 0;
  duration = 0;
  paused = true;
  ended = false;
  readyState = 0;
  playCalls = 0;
  pauseCalls = 0;
  loadCalls = 0;
  removedAttributes: string[] = [];
  playRejection: Error | null = null;
  private readonly listeners = new Map<string, Set<() => void>>();

  addEventListener(type: string, listener: () => void): void {
    const set = this.listeners.get(type) ?? new Set();
    set.add(listener);
    this.listeners.set(type, set);
  }

  removeEventListener(type: string, listener: () => void): void {
    this.listeners.get(type)?.delete(listener);
  }

  emit(type: string): void {
    for (const listener of [...(this.listeners.get(type) ?? [])]) listener();
  }

  // 模拟浏览器完成准备：元数据 + 可播放。
  prepared(duration = 240): void {
    this.duration = duration;
    this.readyState = 4;
    this.emit('loadedmetadata');
    this.emit('canplay');
  }

  load(): void {
    this.loadCalls += 1;
  }

  play(): Promise<void> {
    this.playCalls += 1;
    if (this.playRejection) return Promise.reject(this.playRejection);
    this.paused = false;
    return Promise.resolve();
  }

  pause(): void {
    this.pauseCalls += 1;
    this.paused = true;
    this.emit('pause');
  }

  removeAttribute(name: string): void {
    this.removedAttributes.push(name);
    if (name === 'src') this.src = '';
  }
}

function memoryStorage(): StorageLike & { data: Map<string, string> } {
  const data = new Map<string, string>();
  return {
    data,
    getItem: (key) => data.get(key) ?? null,
    setItem: (key, value) => {
      data.set(key, value);
    },
    removeItem: (key) => {
      data.delete(key);
    },
  };
}

describe('音频引擎的五个对外方法', () => {
  let engine: PlayerEngine;
  let audio: FakeAudio;

  beforeEach(() => {
    engine = new PlayerEngine();
    audio = new FakeAudio();
    engine.attach(audio);
  });

  afterEach(() => {
    engine.detach();
  });

  it('load 只选定媒体并进入准备状态，不会自行 play', async () => {
    await engine.load(track('7'));
    expect(audio.playCalls).toBe(0);
    expect(audio.src).toBe('/media/tracks/7');
    expect(audio.loadCalls).toBe(1);
    expect(engine.snapshot.state).toBe('loading');
    expect(engine.snapshot.track?.id).toBe('7');
  });

  it('媒体准备完成后停在 paused，等待用户动作', async () => {
    await engine.load(track('7'));
    audio.prepared(240);
    expect(audio.playCalls).toBe(0);
    expect(engine.snapshot.state).toBe('paused');
    expect(engine.snapshot.durationMs).toBe(240_000);
  });

  it('不可用的曲目不会去拉媒体流', async () => {
    await engine.load(track('7'));
    await engine.load(track('8', { available: false }));
    expect(audio.src).toBe('');
    expect(audio.removedAttributes).toContain('src');
    expect(engine.snapshot.state).toBe('error');
    expect(engine.snapshot.message).toContain('不可用');
  });

  it('play 成功后状态为 playing', async () => {
    await engine.load(track('7'));
    audio.prepared();
    await engine.play();
    expect(audio.playCalls).toBe(1);
    expect(engine.snapshot.state).toBe('playing');
    expect(engine.snapshot.message).toBeNull();
  });

  it('pause 立即落到 paused', async () => {
    await engine.load(track('7'));
    await engine.play();
    engine.pause();
    expect(engine.snapshot.state).toBe('paused');
    expect(audio.pauseCalls).toBeGreaterThan(0);
  });

  it('浏览器拒绝自动播放时落到可解释状态且不抛未捕获 rejection', async () => {
    const unhandled: unknown[] = [];
    const record = (reason: unknown) => unhandled.push(reason);
    nodeProcess.on('unhandledRejection', record);
    await engine.load(track('7'));
    audio.prepared();
    audio.playRejection = Object.assign(new Error('play() failed'), { name: 'NotAllowedError' });

    await engine.play();
    await new Promise((resolve) => setTimeout(resolve, 0));
    nodeProcess.off('unhandledRejection', record);

    expect(unhandled).toEqual([]);
    expect(engine.snapshot.state).toBe('paused');
    expect(engine.snapshot.track?.id).toBe('7');
    expect(engine.snapshot.message).toContain('浏览器拒绝了自动播放');
  });

  it('媒体本身放不出来时是 error 而不是静默停在 paused', async () => {
    await engine.load(track('7'));
    audio.playRejection = Object.assign(new Error('no supported source'), {
      name: 'NotSupportedError',
    });
    await engine.play();
    expect(engine.snapshot.state).toBe('error');
    expect(engine.snapshot.message).toContain('无法开始播放');
  });

  it('seek 用秒并钳到 [0, duration]', async () => {
    await engine.load(track('7'));
    audio.prepared(10);
    engine.seek(4);
    expect(audio.currentTime).toBe(4);
    engine.seek(99);
    expect(audio.currentTime).toBe(10);
    engine.seek(-5);
    expect(audio.currentTime).toBe(0);
  });

  it('元数据未到时 seek 先记意图，loadedmetadata 后落位', async () => {
    await engine.load(track('7'));
    engine.seek(42);
    expect(audio.currentTime).toBe(0);
    audio.prepared(240);
    expect(audio.currentTime).toBe(42);
  });

  it('dispose 释放媒体、回到 idle 并忽略迟到事件', async () => {
    await engine.load(track('7'));
    await engine.play();
    engine.dispose();
    expect(audio.src).toBe('');
    expect(audio.removedAttributes).toContain('src');
    expect(engine.snapshot.state).toBe('idle');
    expect(engine.snapshot.track).toBeNull();
    expect(engine.snapshot.positionMs).toBe(0);
    audio.prepared();
    audio.emit('loadstart');
    expect(engine.snapshot.state).toBe('idle');
  });

  it('重复 attach 同一元素不会重置正在播放的状态', async () => {
    await engine.load(track('7'));
    await engine.play();
    engine.attach(audio);
    expect(engine.snapshot.state).toBe('playing');
    expect(audio.playCalls).toBe(1);
    expect(audio.src).toBe('/media/tracks/7');
  });

  it('ended 事件只通知一次接播回调', async () => {
    let ended = 0;
    engine.onEnded(() => {
      ended += 1;
    });
    await engine.load(track('7'));
    await engine.play();
    audio.ended = true;
    audio.emit('ended');
    expect(ended).toBe(1);
    expect(engine.snapshot.state).toBe('idle');
  });
});

describe('播放队列', () => {
  const make = (count: number) =>
    Array.from({ length: count }, (_, index) => track(String(index + 1)));

  it('加入、下一首、移除、清空', () => {
    const queue = new PlaybackQueue();
    queue.add(make(2));
    expect(queue.size).toBe(2);
    queue.setCursorIndex(0);
    queue.playNext(track('next'));
    expect(queue.order).toEqual(['1', 'next', '2']);
    expect(queue.removeAt(1)?.id).toBe('next');
    expect(queue.order).toEqual(['1', '2']);
    expect(queue.remove('2')).toBe(1);
    expect(queue.size).toBe(1);
    queue.clear();
    expect(queue.size).toBe(0);
    expect(queue.currentTrack).toBeNull();
  });

  it('移除当前项后光标停在同位置的下一首', () => {
    const queue = new PlaybackQueue();
    queue.add(make(3));
    queue.setCursorIndex(1);
    queue.removeAt(1);
    expect(queue.currentTrack?.id).toBe('3');
    expect(queue.currentIndex).toBe(1);
  });

  it('超过 1000 项时拒绝并标记截断', () => {
    const queue = new PlaybackQueue();
    const ids = Array.from({ length: QUEUE_LIMIT + 5 }, (_, index) => String(index));
    const accepted = queue.addIds(ids);
    expect(accepted).toBe(false);
    expect(queue.size).toBe(QUEUE_LIMIT);
    expect(queue.truncated).toBe(true);
    expect(queue.add(track('extra'))).toBe(false);
    expect(queue.size).toBe(QUEUE_LIMIT);
  });

  it('自定义上限同样受约束', () => {
    const queue = new PlaybackQueue({ limit: 2 });
    expect(queue.add(make(3))).toBe(false);
    expect(queue.size).toBe(2);
  });

  it('随机播放只打乱未播放部分的 ID 顺序，不复制也不改详情', () => {
    const tracks = make(6);
    const queue = new PlaybackQueue({ random: () => 0.5 });
    queue.add(tracks);
    queue.setCursorIndex(0);
    const before = [...queue.order];
    queue.toggleShuffle();
    const after = [...queue.order];

    expect(after.length).toBe(before.length);
    expect([...after].sort()).toEqual([...before].sort());
    expect(after[0]).toBe(before[0]);
    expect(after).toEqual(expect.arrayContaining(before));
    expect(after.join(',')).not.toBe(before.join(','));
    // 详情仍按 ID 解析，打乱不会把标题错配到别的曲目上。
    expect(queue.tracksInOrder.map((item) => item.id)).toEqual(after);
    expect(queue.tracksInOrder.every((item) => item.title === `曲目 ${item.id}`)).toBe(true);
  });

  it('单曲循环只返回当前曲目，不复制队列', () => {
    const queue = new PlaybackQueue();
    queue.add(make(3));
    queue.setCursorIndex(1);
    queue.toggleRepeatOne();
    const before = [...queue.order];
    expect(queue.next()?.id).toBe('2');
    expect(queue.next()?.id).toBe('2');
    expect(queue.size).toBe(3);
    expect([...queue.order]).toEqual(before);
    queue.toggleRepeatOne();
    expect(queue.next()?.id).toBe('3');
  });

  it('队列到末尾时 next 返回 null', () => {
    const queue = new PlaybackQueue();
    queue.add(make(1));
    queue.setCursorIndex(0);
    expect(queue.next()).toBeNull();
  });

  it('只拿到 ID 时先占位，补详情后不丢顺序', () => {
    const queue = new PlaybackQueue();
    queue.addIds(['9', '10']);
    expect(queue.missingDetails()).toEqual(['9', '10']);
    expect(queue.trackAt(0)).toEqual(placeholderTrack('9'));
    queue.applyDetails([track('9')]);
    expect(queue.trackAt(0)?.title).toBe('曲目 9');
    expect(queue.missingDetails()).toEqual(['10']);
  });
});

describe('刷新恢复与全局单例', () => {
  it('恢复队列与位置，但绝不自动播放', async () => {
    const storage = memoryStorage();
    const first = new PlayerStore(storage);
    const audio = new FakeAudio();
    first.attachAudio(audio);
    await first.play(track('1'));
    audio.prepared(240);
    first.enqueue(track('2'));
    first.seek(42);
    first.persist();

    const second = new PlayerStore(storage);
    const revived = new FakeAudio();
    second.attachAudio(revived);
    await second.restore();

    expect(revived.playCalls).toBe(0);
    expect(second.queue.size).toBe(2);
    expect(second.queue.currentTrack?.id).toBe('1');
    expect(second.snapshot.player.state).not.toBe('playing');
    expect(second.snapshot.player.track?.id).toBe('1');
    expect(second.snapshot.player.positionMs).toBe(42_000);
  });

  it('退出登录清空持久化队列，刷新后无从恢复', async () => {
    const storage = memoryStorage();
    const store = new PlayerStore(storage);
    store.attachAudio(new FakeAudio());
    await store.play(track('1'));
    store.forget();

    const after = new PlayerStore(storage);
    const audio = new FakeAudio();
    after.attachAudio(audio);
    await expect(after.restore()).resolves.toBe(false);
    expect(audio.playCalls).toBe(0);
    expect(after.queue.size).toBe(0);
  });

  it('队列模式与截断标记随刷新保留', async () => {
    const storage = memoryStorage();
    const store = new PlayerStore(storage);
    store.attachAudio(new FakeAudio());
    await store.enqueue(track('1'));
    store.toggleShuffle();
    store.toggleRepeatOne();
    const revived = new PlayerStore(storage);
    await revived.restore();
    expect(revived.queue.shuffle).toBe(true);
    expect(revived.queue.repeatOne).toBe(true);
  });

  it('损坏的持久化数据按无历史处理', async () => {
    const storage = memoryStorage();
    storage.setItem('litebeat.player.v1', '{ 坏掉的 JSON');
    const store = new PlayerStore(storage);
    const audio = new FakeAudio();
    store.attachAudio(audio);
    await expect(store.restore()).resolves.toBe(false);
    expect(audio.playCalls).toBe(0);
    expect(store.queue.size).toBe(0);
  });

  it('自然播放结束自动接播下一首且不清空队列', async () => {
    const store = new PlayerStore(memoryStorage());
    const audio = new FakeAudio();
    store.attachAudio(audio);
    await store.enqueue(track('1'));
    await store.enqueue(track('2'));
    await store.playAt(0);
    await store.skipNext();
    expect(store.queue.size).toBe(2);
    expect(store.queue.currentTrack?.id).toBe('2');
    expect(audio.playCalls).toBe(2);
  });
});

describe('搜索控制器', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  const page = (ids: string[], hasMore = false, nextCursor: string | null = null): SearchPage => ({
    items: ids.map((id) => track(id)),
    next_cursor: nextCursor,
    has_more: hasMore,
  });

  it('防抖窗口内不请求，停止输入后才发一次', async () => {
    const calls: string[] = [];
    const controller = new SearchController({
      debounceMs: 250,
      fetcher: async (query) => {
        calls.push(query);
        return page(['1']);
      },
    });
    controller.setText('周');
    await vi.advanceTimersByTimeAsync(120);
    controller.setText('周杰');
    await vi.advanceTimersByTimeAsync(200);
    expect(calls).toEqual([]);
    await vi.advanceTimersByTimeAsync(60);
    expect(calls).toEqual(['周杰']);
  });

  it('输入法合成期间不搜索，合成结束再搜一次', async () => {
    const calls: string[] = [];
    const controller = new SearchController({
      debounceMs: 250,
      fetcher: async (query) => {
        calls.push(query);
        return page(['1']);
      },
    });
    controller.setComposing(true);
    controller.setText('周');
    await vi.advanceTimersByTimeAsync(1000);
    controller.setText('周杰伦');
    await vi.advanceTimersByTimeAsync(1000);
    expect(calls).toEqual([]);
    expect(controller.snapshot.phase).toBe('composing');

    controller.setComposing(false);
    await vi.advanceTimersByTimeAsync(250);
    expect(calls).toEqual(['周杰伦']);
  });

  it('新查询会 abort 上一个在途请求', async () => {
    const inFlight: { query: string; signal: AbortSignal }[] = [];
    const controller = new SearchController({
      debounceMs: 0,
      fetcher: (query, options) => {
        inFlight.push({ query, signal: options.signal });
        // 不自行 resolve：请求一直留在途，直到测试或 abort 结束它。
        return new Promise<SearchPage>(() => undefined);
      },
    });
    controller.setText('第一个');
    await vi.advanceTimersByTimeAsync(1);
    expect(inFlight.map((entry) => entry.query)).toEqual(['第一个']);
    controller.setText('第二个');
    await vi.advanceTimersByTimeAsync(1);
    expect(inFlight.map((entry) => entry.query)).toEqual(['第一个', '第二个']);
    expect(inFlight[0].signal.aborted).toBe(true);
    expect(inFlight[1].signal.aborted).toBe(false);
    controller.cancel();
    expect(inFlight[1].signal.aborted).toBe(true);
  });

  it('后到的旧响应被丢弃，不覆盖新结果', async () => {
    const resolvers = new Map<string, (value: SearchPage) => void>();
    const controller = new SearchController({
      debounceMs: 0,
      fetcher: (query) =>
        new Promise((resolve) => {
          resolvers.set(query, resolve);
        }),
    });
    controller.setText('旧查询');
    await vi.advanceTimersByTimeAsync(1);
    controller.setText('新查询');
    await vi.advanceTimersByTimeAsync(1);

    resolvers.get('新查询')?.(page(['new-1']));
    await vi.advanceTimersByTimeAsync(0);
    expect(controller.snapshot.items.map((item) => item.id)).toEqual(['new-1']);

    resolvers.get('旧查询')?.(page(['stale-1', 'stale-2']));
    await vi.advanceTimersByTimeAsync(0);
    expect(controller.snapshot.items.map((item) => item.id)).toEqual(['new-1']);
    expect(controller.snapshot.dropped).toBe(1);
    expect(controller.snapshot.phase).toBe('ready');
  });

  it('超过 64 字符直接提示，不打后端', async () => {
    const calls: string[] = [];
    const controller = new SearchController({
      debounceMs: 0,
      fetcher: async (query) => {
        calls.push(query);
        return page([]);
      },
    });
    controller.setText('周'.repeat(65));
    await vi.advanceTimersByTimeAsync(10);
    expect(calls).toEqual([]);
    expect(controller.snapshot.phase).toBe('too-long');
    expect(controller.snapshot.message).toContain('64');
  });

  it('清空输入即清空结果且不发请求', async () => {
    const calls: string[] = [];
    const controller = new SearchController({
      debounceMs: 0,
      fetcher: async (query) => {
        calls.push(query);
        return page(['1']);
      },
    });
    controller.setText('周');
    await vi.advanceTimersByTimeAsync(1);
    controller.setText('   ');
    await vi.advanceTimersByTimeAsync(20);
    expect(calls).toEqual(['周']);
    expect(controller.snapshot.items).toEqual([]);
    expect(controller.snapshot.phase).toBe('blank');
  });

  it('加载更多沿用 next_cursor 并追加结果', async () => {
    const seen: (string | null | undefined)[] = [];
    const controller = new SearchController({
      debounceMs: 0,
      fetcher: async (_query, options) => {
        seen.push(options.cursor);
        return options.cursor ? page(['3'], false, null) : page(['1', '2'], true, 'c2');
      },
    });
    controller.setText('周');
    await vi.advanceTimersByTimeAsync(1);
    expect(controller.snapshot.has_more).toBe(true);
    await controller.loadMore();
    expect(seen).toEqual([null, 'c2']);
    expect(controller.snapshot.items.map((item) => item.id)).toEqual(['1', '2', '3']);
    expect(controller.snapshot.has_more).toBe(false);
  });

  it('搜索失败时保留错误码与请求号', async () => {
    const controller = new SearchController({
      debounceMs: 0,
      fetcher: async () => {
        throw new ApiError(503, 'QUERY_TIMEOUT', '查询超时', 'req-42');
      },
    });
    controller.setText('周');
    await vi.advanceTimersByTimeAsync(1);
    expect(controller.snapshot.phase).toBe('error');
    expect(controller.snapshot.message).toContain('QUERY_TIMEOUT');
    expect(controller.snapshot.message).toContain('req-42');
  });
});

describe('游标分页器', () => {
  it('INVALID_CURSOR 时丢弃本地游标重拉首页，只重一次', async () => {
    const seen: (string | null)[] = [];
    let rejectSecond = true;
    const paginator = new Paginator<TrackSummary>({
      fetch: async (cursor) => {
        seen.push(cursor);
        if (seen.length === 1) return { items: [track('1')], next_cursor: 'bad', has_more: true };
        if (rejectSecond && cursor === 'bad') {
          rejectSecond = false;
          throw new ApiError(400, 'INVALID_CURSOR', '游标不匹配', 'req-7');
        }
        return { items: [track('fresh')], next_cursor: null, has_more: false };
      },
    });
    await paginator.load();
    await paginator.loadMore();
    expect(seen).toEqual([null, 'bad', null]);
    expect(paginator.snapshot.items.map((item) => item.id)).toEqual(['fresh']);
    expect(paginator.snapshot.phase).toBe('ready');
  });

  it('普通失败落到 error 并带上 code 与 request_id', async () => {
    const paginator = new Paginator<TrackSummary>({
      fetch: async () => {
        throw new ApiError(401, 'UNAUTHORIZED', '需要登录', 'req-9');
      },
    });
    await paginator.load();
    expect(paginator.snapshot.phase).toBe('error');
    expect(paginator.snapshot.code).toBe('UNAUTHORIZED');
    expect(paginator.snapshot.requestId).toBe('req-9');
  });

  it('空库显示空状态而不是错误', async () => {
    const paginator = new Paginator<TrackSummary>({
      fetch: async () => ({ items: [], next_cursor: null, has_more: false }),
    });
    await paginator.load();
    expect(paginator.snapshot.phase).toBe('empty');
  });
});

describe('媒体地址与契约', () => {
  it('占位曲目只带 ID，标题留空由界面兜底', () => {
    expect(placeholderTrack('12')).toEqual({
      id: '12',
      title: '',
      artist: '',
      album_id: null,
      duration_ms: null,
      available: true,
    });
  });
});
