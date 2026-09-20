<script lang="ts">
  import type { ListPhase } from '../pagination';
  import type { TrackSummary } from '../contracts';
  import { player } from '../player/state';
  import { formatDuration } from '../time';

  // 列表只负责展示与派发用户动作；正在播放哪一首取自全局播放器快照，避免各页各自记状态。
  let {
    tracks,
    phase = 'ready',
    message = null,
    code = null,
    requestId = null,
    hasMore = false,
    loadingMore = false,
    emptyText = '这里还没有曲目',
    onMore,
    onRetry,
  }: {
    tracks: TrackSummary[];
    phase?: ListPhase;
    message?: string | null;
    code?: string | null;
    requestId?: string | null;
    hasMore?: boolean;
    loadingMore?: boolean;
    emptyText?: string;
    onMore?: () => void;
    onRetry?: () => void;
  } = $props();

  let playingId = $state<string | null>(null);
  let playState = $state('idle');

  $effect(() => player.subscribe((next) => {
    playingId = next.player.track?.id ?? null;
    playState = next.player.state;
  }));

  function titleOf(track: TrackSummary): string {
    return track.title || `曲目 #${track.id}`;
  }
</script>

{#if phase === 'loading'}
  <p class="state" data-testid="track-list-state">正在读取曲库…</p>
{:else if phase === 'error'}
  <div class="state error" data-testid="track-list-state" role="alert">
    <p>{message ?? '列表加载失败'}</p>
    {#if code}
      <p class="code">{code}{requestId ? ` · 请求号 ${requestId}` : ''}</p>
    {/if}
    {#if onRetry}
      <button type="button" onclick={() => onRetry()}>重新加载</button>
    {/if}
  </div>
{:else if tracks.length === 0}
  <p class="state" data-testid="track-list-state">{emptyText}</p>
{:else}
  <ul class="list">
    {#each tracks as track (track.id)}
      {@const current = track.id === playingId}
      <li class="row" class:current data-track-id={track.id}>
        <button
          type="button"
          class="play"
          data-testid="track-play"
          disabled={!track.available}
          aria-label={`播放 ${titleOf(track)}`}
          title={track.available ? '播放' : '文件缺失，无法播放'}
          onclick={() => void player.play(track)}
        >
          {current && playState === 'playing' ? '❚' : '▶'}
        </button>
        <span class="main">
          <span class="title">{titleOf(track)}</span>
          <span class="sub">
            {track.artist || '未知歌手'}
            {#if !track.available}· 不可用{/if}
          </span>
        </span>
        <span class="dur">{formatDuration(track.duration_ms)}</span>
        <span class="acts">
          <button type="button" data-testid="track-enqueue" onclick={() => void player.enqueue(track)}>
            加入队列
          </button>
          <button
            type="button"
            data-testid="track-next"
            onclick={() => void player.enqueueNext(track)}
          >
            下一首播放
          </button>
        </span>
      </li>
    {/each}
  </ul>
  <div class="more">
    {#if hasMore}
      <button type="button" data-testid="load-more" disabled={loadingMore} onclick={() => onMore?.()}>
        {loadingMore ? '加载中…' : '加载更多'}
      </button>
    {:else}
      <span class="end">共 {tracks.length} 首，已到末尾</span>
    {/if}
  </div>
{/if}

<style>
  .state {
    color: #aab6c2;
    background: #192129;
    border-radius: 10px;
    padding: 14px 16px;
    font-size: 0.95rem;
  }

  .error {
    background: #2a1a1a;
    border: 1px solid #5a2b2b;
    color: #f3f6f8;
  }

  .error .code {
    color: #aab6c2;
    font-size: 0.8rem;
  }

  .error button,
  .more button {
    background: #42d3ad;
    color: #101418;
    border: 0;
    border-radius: 8px;
    padding: 9px 14px;
    font-size: 0.9rem;
    font-weight: 600;
    cursor: pointer;
  }

  .more button:disabled {
    background: #2b3743;
    color: #aab6c2;
  }

  .list {
    list-style: none;
    margin: 0;
    padding: 0;
  }

  .row {
    display: grid;
    grid-template-columns: auto 1fr auto;
    grid-template-areas:
      'play main dur'
      'play acts acts';
    gap: 4px 10px;
    align-items: center;
    padding: 10px 8px;
    border-bottom: 1px solid #1d2630;
  }

  .row.current {
    background: #16202a;
  }

  .play {
    grid-area: play;
    width: 44px;
    height: 44px;
    border-radius: 50%;
    border: 1px solid #2b3743;
    background: #192129;
    color: #42d3ad;
    font-size: 0.85rem;
    cursor: pointer;
  }

  .play:disabled {
    color: #55606b;
    cursor: not-allowed;
  }

  .main {
    grid-area: main;
    display: flex;
    flex-direction: column;
    min-width: 0;
  }

  .title {
    color: #f3f6f8;
    font-size: 0.98rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .sub {
    color: #aab6c2;
    font-size: 0.8rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .dur {
    grid-area: dur;
    color: #aab6c2;
    font-size: 0.8rem;
    font-variant-numeric: tabular-nums;
  }

  .acts {
    grid-area: acts;
    display: flex;
    gap: 6px;
  }

  .acts button {
    background: transparent;
    color: #aab6c2;
    border: 1px solid #2b3743;
    border-radius: 6px;
    padding: 4px 8px;
    font-size: 0.75rem;
    cursor: pointer;
  }

  .more {
    display: flex;
    justify-content: center;
    padding: 16px 0 4px;
  }

  .end {
    color: #55606b;
    font-size: 0.8rem;
  }

  @media (min-width: 720px) {
    .row {
      grid-template-columns: auto 1fr auto auto;
      grid-template-areas: 'play main dur acts';
    }
  }
</style>
