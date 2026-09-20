<script lang="ts">
  import TrackList from '../lib/components/TrackList.svelte';
  import { listTracks, queueIds } from '../lib/api';
  import type { TrackSummary } from '../lib/contracts';
  import { navigate } from '../lib/router';
  import { player } from '../lib/player/state';

  let recent = $state<TrackSummary[]>([]);
  let phase = $state<'loading' | 'ready' | 'empty' | 'error'>('loading');
  let message = $state<string | null>(null);
  let notice = $state<string | null>(null);
  let loadingAll = $state(false);

  $effect(() => {
    const controller = new AbortController();
    phase = 'loading';
    void listTracks({ limit: 10, sort: 'recent', signal: controller.signal })
      .then((page) => {
        recent = page.items;
        phase = page.items.length === 0 ? 'empty' : 'ready';
      })
      .catch((caught: unknown) => {
        if (controller.signal.aborted) return;
        phase = 'error';
        message = caught instanceof Error ? caught.message : '首页加载失败';
      });
    return () => controller.abort();
  });

  async function playEverything(): Promise<void> {
    loadingAll = true;
    notice = null;
    try {
      const result = await queueIds('library');
      await player.enqueueIds(result.ids, result.truncated);
      if (result.ids.length > 0) await player.playAt(0);
    } catch (caught) {
      notice = caught instanceof Error ? caught.message : '无法载入完整曲库';
    } finally {
      loadingAll = false;
    }
  }
</script>

<svelte:head>
  <title>LiteBeat · 我的音乐库</title>
</svelte:head>

<section class="hero">
  <p class="eyebrow">YOUR MUSIC, YOUR SPACE</p>
  <h1>把音乐库留在自己的设备上。</h1>
  <p>扫描本地目录，在浏览器里检索、播放和整理自己的收藏。队列最多 1000 首。</p>
  <div class="cta">
    <button type="button" data-testid="play-all" disabled={loadingAll} onclick={() => void playEverything()}>
      {loadingAll ? '准备中…' : '播放全部'}
    </button>
    <button type="button" class="ghost" onclick={() => navigate('/library')}>浏览曲目</button>
    <button type="button" class="ghost" onclick={() => navigate('/albums')}>按专辑浏览</button>
  </div>
  {#if notice}
    <p class="warn" role="status">{notice}</p>
  {/if}
</section>

<section class="recent">
  <h2>最近入库</h2>
  <TrackList tracks={recent} {phase} {message} emptyText="曲库还是空的，先在管理端跑一次扫描。" />
  <p class="tip">提示：点曲目右侧「加入队列」可以攒列表，「下一首播放」会插在当前曲目之后。</p>
</section>

<style>
  .hero {
    max-width: 680px;
    padding: 8px 0 24px;
  }

  .eyebrow {
    color: #42d3ad;
    font-size: 0.72rem;
    letter-spacing: 0.14em;
    margin: 0;
  }

  h1 {
    font-size: clamp(1.75rem, 6vw, 3.25rem);
    line-height: 1.1;
    margin: 10px 0;
    color: #f3f6f8;
  }

  .hero p {
    color: #aab6c2;
    font-size: 1rem;
    line-height: 1.7;
  }

  .cta {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
    margin-top: 14px;
  }

  .cta button {
    background: #42d3ad;
    color: #101418;
    border: 0;
    border-radius: 10px;
    padding: 11px 16px;
    font-size: 0.95rem;
    font-weight: 600;
    cursor: pointer;
  }

  .cta .ghost {
    background: transparent;
    color: #aab6c2;
    border: 1px solid #2b3743;
    font-weight: 400;
  }

  .cta button:disabled {
    background: #2b3743;
    color: #aab6c2;
  }

  .warn {
    color: var(--danger);
    font-size: 0.85rem;
  }

  h2 {
    color: #f3f6f8;
    font-size: 1.1rem;
    margin: 0 0 10px;
  }

  .tip {
    color: #55606b;
    font-size: 0.8rem;
  }
</style>
