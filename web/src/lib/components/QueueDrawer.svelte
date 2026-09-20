<script lang="ts">
  import { player, type StoreSnapshot } from '../player/state';
  import { formatDuration } from '../time';

  let { snapshot, onClose }: { snapshot: StoreSnapshot; onClose: () => void } = $props();

  // 队列可能只有 ID（播放全部走 queue-ids），打开抽屉时按需补详情，一次最多 50 首。
  $effect(() => {
    void player.hydratePending();
  });
</script>

<aside class="drawer" data-testid="queue-drawer" aria-label="播放队列">
  <div class="head">
    <strong>播放队列</strong>
    <span class="count">{snapshot.queue.size} / {snapshot.queue.limit}</span>
    <button type="button" onclick={onClose}>关闭</button>
  </div>

  {#if snapshot.queue.truncated}
    <p class="warn" data-testid="queue-truncated" role="status">
      队列已达上限，超出部分未加入。
    </p>
  {/if}

  {#if snapshot.queue.tracks.length === 0}
    <p class="empty">队列是空的。在列表里点「加入队列」，或用「播放全部」一次入队。</p>
  {:else}
    <ol class="list">
      {#each snapshot.queue.tracks as track, index (index)}
        {@const current = index === snapshot.queue.current}
        <li class="item" class:current data-testid="queue-item">
          <button
            type="button"
            class="go"
            title="从这一首开始播"
            aria-label={`从「${track.title || `曲目 #${track.id}`}」开始播`}
            onclick={() => void player.playAt(index)}
          >
            {current && snapshot.player.state === 'playing' ? '❚' : '▶'}
          </button>
          <span class="meta">
            <span class="title">{track.title || `曲目 #${track.id}`}</span>
            <span class="sub">{track.artist || '未知歌手'} · {formatDuration(track.duration_ms)}</span>
          </span>
          <button
            type="button"
            class="rm"
            data-testid="queue-remove"
            aria-label="移出队列"
            onclick={() => player.removeAt(index)}
          >
            移
          </button>
        </li>
      {/each}
    </ol>
    <div class="foot">
      <button
        type="button"
        data-testid="queue-shuffle"
        class:on={snapshot.queue.shuffle}
        onclick={() => player.toggleShuffle()}
      >
        随机
      </button>
      <button
        type="button"
        data-testid="queue-repeat"
        class:on={snapshot.queue.repeatOne}
        onclick={() => player.toggleRepeatOne()}
      >
        单曲循环
      </button>
      <button type="button" data-testid="queue-clear" onclick={() => player.clearQueue()}>
        清空队列
      </button>
    </div>
  {/if}
</aside>

<style>
  .drawer {
    position: fixed;
    top: 56px;
    right: 8px;
    bottom: 92px;
    left: 8px;
    z-index: 40;
    display: flex;
    flex-direction: column;
    background: #141a20;
    border: 1px solid #2b3743;
    border-radius: 12px;
    padding: 12px;
    box-sizing: border-box;
  }

  .head {
    display: flex;
    align-items: center;
    gap: 10px;
    margin-bottom: 8px;
  }

  .head strong {
    flex: 1;
    color: #f3f6f8;
    font-size: 1rem;
  }

  .head button,
  .foot button,
  .rm {
    background: transparent;
    color: #aab6c2;
    border: 1px solid #2b3743;
    border-radius: 8px;
    padding: 5px 9px;
    font-size: 0.78rem;
    cursor: pointer;
  }

  .count {
    color: #aab6c2;
    font-size: 0.78rem;
    font-variant-numeric: tabular-nums;
  }

  .warn {
    margin: 0 0 8px;
    color: #101418;
    background: #ffcf6b;
    border-radius: 8px;
    padding: 8px 10px;
    font-size: 0.8rem;
  }

  .empty {
    color: #aab6c2;
    font-size: 0.9rem;
  }

  .list {
    list-style: none;
    margin: 0;
    padding: 0;
    overflow: auto;
    flex: 1;
    counter-reset: q;
  }

  .item {
    display: grid;
    grid-template-columns: auto 1fr auto;
    gap: 8px;
    align-items: center;
    padding: 7px 4px;
    border-bottom: 1px solid #1d2630;
  }

  .item.current .title {
    color: #42d3ad;
  }

  .go {
    width: 30px;
    height: 30px;
    border-radius: 50%;
    border: 1px solid #2b3743;
    background: #192129;
    color: #42d3ad;
    cursor: pointer;
    font-size: 0.75rem;
  }

  .meta {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }

  .title {
    color: #f3f6f8;
    font-size: 0.88rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .sub {
    color: #aab6c2;
    font-size: 0.72rem;
  }

  .foot {
    display: flex;
    gap: 6px;
    padding-top: 10px;
  }

  .foot button.on {
    color: #101418;
    background: #42d3ad;
    border-color: #42d3ad;
  }

  @media (min-width: 720px) {
    .drawer {
      left: auto;
      width: 340px;
    }
  }
</style>
