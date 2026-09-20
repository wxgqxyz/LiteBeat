<script lang="ts">
  import { player, type StoreSnapshot } from '../player/state';
  import { formatDuration } from '../time';

  let { snapshot, onExpand }: { snapshot: StoreSnapshot; onExpand: () => void } = $props();

  let dragging = $state(false);
  let dragSeconds = $state(0);

  const track = $derived(snapshot.player.track);
  const durationMs = $derived(snapshot.player.durationMs ?? track?.duration_ms ?? 0);
  const seconds = $derived(durationMs > 0 ? durationMs / 1000 : 0);
  const shown = $derived(dragging ? dragSeconds : snapshot.player.positionMs / 1000);
  // max 不能低于当前值：浏览器会把越界的 range value 夹住，而表达式值没变时 Svelte
  // 不会重新赋值，于是「位置先到、时长后到」（刷新恢复就是这样）滑块会永久卡在最左。
  const limit = $derived(Math.max(seconds, shown, 1));

  const LABEL: Record<string, string> = {
    idle: '空闲',
    loading: '准备中',
    playing: '播放中',
    paused: '已暂停',
    buffering: '缓冲中',
    error: '出错',
  };
</script>

{#if track}
  <div class="bar" data-testid="player-bar">
    <button type="button" class="art" onclick={onExpand} aria-label="展开播放器">
      {snapshot.player.state === 'playing' ? '❚❚' : '▶'}
    </button>
    <span class="meta">
      <button type="button" class="title" onclick={onExpand}>
        {track.title || `曲目 #${track.id}`}
      </button>
      <span class="sub">
        {track.artist || '未知歌手'}
        <span class="state" data-testid="player-state">{snapshot.player.state}</span>
      </span>
    </span>

    <div class="seek">
      <span class="time">{formatDuration(shown * 1000)}</span>
      <input
        type="range"
        data-testid="player-seek"
        class="slider"
        min="0"
        max={limit}
        step="0.1"
        value={shown}
        disabled={durationMs <= 0}
        aria-label="播放位置"
        oninput={(event) => {
          dragging = true;
          dragSeconds = Number((event.currentTarget as HTMLInputElement).value);
        }}
        onchange={() => {
          player.seek(dragSeconds);
          dragging = false;
        }}
      />
      <span class="time">{formatDuration(durationMs || null)}</span>
    </div>

    <div class="ctl">
      <button
        type="button"
        data-testid="toggle-shuffle"
        class:on={snapshot.queue.shuffle}
        title="随机播放只打乱未播放部分的 ID"
        onclick={() => player.toggleShuffle()}
      >
        随机
      </button>
      <button
        type="button"
        data-testid="toggle-repeat"
        class:on={snapshot.queue.repeatOne}
        title="单曲循环不复制队列"
        onclick={() => player.toggleRepeatOne()}
      >
        单曲
      </button>
      <button
        type="button"
        data-testid="player-prev"
        aria-label="上一首"
        onclick={() => void player.skipPrevious()}
      >
        ⏮
      </button>
      <button
        type="button"
        class="primary"
        data-testid="player-toggle"
        onclick={() => void player.toggle()}
      >
        {snapshot.player.state === 'playing' ? '暂停' : '播放'}
      </button>
      <button
        type="button"
        data-testid="player-next"
        aria-label="下一首"
        onclick={() => void player.skipNext()}
      >
        ⏭
      </button>
    </div>

    {#if snapshot.player.message}
      <p class="message" data-testid="player-message" role="status">
        {snapshot.player.message}（{LABEL[snapshot.player.state] ?? snapshot.player.state}）
      </p>
    {/if}
  </div>
{/if}

<style>
  .bar {
    position: fixed;
    left: 0;
    right: 0;
    bottom: 0;
    z-index: 35;
    display: grid;
    grid-template-columns: auto 1fr auto;
    gap: 8px 12px;
    align-items: center;
    padding: 8px 12px calc(8px + env(safe-area-inset-bottom));
    background: #141a20;
    border-top: 1px solid #2b3743;
    box-sizing: border-box;
  }

  .art {
    width: 40px;
    height: 40px;
    border-radius: 8px;
    border: 1px solid #2b3743;
    background: #192129;
    color: #42d3ad;
    cursor: pointer;
  }

  .meta {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }

  .title {
    background: none;
    border: 0;
    padding: 0;
    text-align: left;
    color: #f3f6f8;
    font-size: 0.92rem;
    cursor: pointer;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .sub {
    display: flex;
    gap: 8px;
    align-items: center;
    color: #aab6c2;
    font-size: 0.76rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .state {
    color: #42d3ad;
    font-size: 0.7rem;
    border: 1px solid #2b3743;
    border-radius: 999px;
    padding: 1px 7px;
  }

  .seek {
    grid-column: 1 / -1;
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .slider {
    flex: 1;
    accent-color: #42d3ad;
    min-height: 28px;
  }

  .time {
    color: #aab6c2;
    font-size: 0.72rem;
    font-variant-numeric: tabular-nums;
    min-width: 40px;
  }

  .ctl {
    grid-column: 1 / -1;
    display: flex;
    gap: 6px;
    justify-content: center;
    flex-wrap: wrap;
  }

  .ctl button {
    background: transparent;
    color: #aab6c2;
    border: 1px solid #2b3743;
    border-radius: 8px;
    padding: 6px 10px;
    font-size: 0.8rem;
    cursor: pointer;
  }

  .ctl button.on {
    color: #101418;
    background: #42d3ad;
    border-color: #42d3ad;
  }

  .ctl .primary {
    min-width: 64px;
    font-weight: 600;
  }

  .message {
    grid-column: 1 / -1;
    margin: 0;
    color: var(--danger);
    font-size: 0.76rem;
  }

  @media (min-width: 900px) {
    .bar {
      grid-template-columns: auto minmax(180px, 1fr) 2fr auto;
    }

    .seek {
      grid-column: auto;
    }

    .ctl {
      grid-column: auto;
    }
  }
</style>
