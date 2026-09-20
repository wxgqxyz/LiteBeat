<script lang="ts">
  import { getTrack } from '../api';
  import type { TrackDetail } from '../contracts';
  import { navigate } from '../router';
  import { player, type StoreSnapshot } from '../player/state';
  import { formatBytes, formatDuration } from '../time';

  // 移动端把播放器拉成全屏：同一引擎、同一音频元素，这里只是另一个视图。
  let { snapshot, onClose }: { snapshot: StoreSnapshot; onClose: () => void } = $props();

  const track = $derived(snapshot.player.track);
  const durationMs = $derived(snapshot.player.durationMs ?? track?.duration_ms ?? 0);
  let detail = $state<TrackDetail | null>(null);

  $effect(() => {
    const id = track?.id;
    detail = null;
    if (!id) return;
    const controller = new AbortController();
    void getTrack(id, { signal: controller.signal })
      .then((next) => (detail = next))
      .catch(() => {
        // 详情只用于展示元数据，失败时留空即可，不打断播放。
      });
    return () => controller.abort();
  });
</script>

<div class="sheet" role="dialog" aria-label="播放器" data-testid="player-sheet">
  <button type="button" class="close" onclick={onClose}>收起</button>
  {#if track}
    <p class="state" data-testid="sheet-state">{snapshot.player.state}</p>
    <h2>{track.title || `曲目 #${track.id}`}</h2>
    <p class="artist">{track.artist || '未知歌手'}</p>
    {#if detail?.album_title}
      <button
        type="button"
        class="album"
        onclick={() => {
          if (detail?.album_id) navigate(`/albums/${encodeURIComponent(detail.album_id)}`);
          onClose();
        }}
      >
        专辑：{detail.album_title}
      </button>
    {/if}

    <input
      type="range"
      class="slider"
      min="0"
      max={durationMs > 0 ? durationMs / 1000 : 1}
      step="0.1"
      value={snapshot.player.positionMs / 1000}
      disabled={durationMs <= 0}
      aria-label="播放位置"
      onchange={(event) => player.seek(Number((event.currentTarget as HTMLInputElement).value))}
    />
    <p class="time">
      {formatDuration(snapshot.player.positionMs)} / {formatDuration(durationMs || null)}
    </p>

    {#if detail}
      <dl class="meta">
        <dt>编码</dt>
        <dd>{detail.codec ?? detail.mime ?? '未知'}</dd>
        <dt>大小</dt>
        <dd>{formatBytes(detail.size_bytes)}</dd>
        <dt>时长</dt>
        <dd>{formatDuration(detail.duration_ms)}</dd>
      </dl>
    {/if}

    {#if snapshot.player.message}
      <p class="message" role="status">{snapshot.player.message}</p>
    {/if}

    <div class="ctl">
      <button type="button" onclick={() => void player.skipPrevious()}>上一首</button>
      <button type="button" class="primary" onclick={() => void player.toggle()}>
        {snapshot.player.state === 'playing' ? '暂停' : '播放'}
      </button>
      <button type="button" onclick={() => void player.skipNext()}>下一首</button>
      <button type="button" class="ghost" onclick={() => player.forget()}>停止并释放</button>
    </div>
  {/if}
</div>

<style>
  .sheet {
    position: fixed;
    inset: 0;
    z-index: 50;
    background: #101418;
    color: #f3f6f8;
    padding: 20px 20px calc(84px + env(safe-area-inset-bottom));
    overflow: auto;
    box-sizing: border-box;
  }

  .close {
    background: transparent;
    color: #aab6c2;
    border: 1px solid #2b3743;
    border-radius: 8px;
    padding: 6px 10px;
    cursor: pointer;
  }

  .state {
    color: #42d3ad;
    font-size: 0.75rem;
    letter-spacing: 0.08em;
  }

  h2 {
    margin: 4px 0;
    font-size: 1.5rem;
    word-break: break-word;
  }

  .artist,
  .time,
  .message {
    color: #aab6c2;
    font-size: 0.9rem;
    margin: 4px 0;
  }

  .album {
    background: transparent;
    border: 0;
    color: #42d3ad;
    padding: 0;
    font-size: 0.85rem;
    cursor: pointer;
  }

  .slider {
    width: 100%;
    accent-color: #42d3ad;
    margin-top: 18px;
  }

  .meta {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 4px 12px;
    font-size: 0.82rem;
    margin: 16px 0;
  }

  .meta dt {
    color: #aab6c2;
  }

  .meta dd {
    margin: 0;
  }

  .message {
    color: var(--danger);
  }

  .ctl {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
    margin-top: 12px;
  }

  .ctl button {
    background: #192129;
    color: #f3f6f8;
    border: 1px solid #2b3743;
    border-radius: 10px;
    padding: 12px 16px;
    font-size: 0.95rem;
    cursor: pointer;
  }

  .ctl .primary {
    background: #42d3ad;
    color: #101418;
    border-color: #42d3ad;
    font-weight: 600;
  }

  .ctl .ghost {
    color: #aab6c2;
  }

  @media (min-width: 720px) {
    .sheet {
      inset: auto 16px 84px 16px;
      max-width: 520px;
      margin: 0 auto;
      border: 1px solid #2b3743;
      border-radius: 14px;
      padding-bottom: 20px;
    }
  }
</style>
