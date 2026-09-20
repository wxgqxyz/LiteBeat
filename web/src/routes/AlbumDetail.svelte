<script lang="ts">
  import TrackList from '../lib/components/TrackList.svelte';
  import { getTrack, listAlbumTracks, queueIds } from '../lib/api';
  import type { TrackSummary } from '../lib/contracts';
  import { Paginator, type ListState } from '../lib/pagination';
  import { navigate, type Route } from '../lib/router';
  import { player } from '../lib/player/state';

  let { route }: { route: Route } = $props();

  const PAGE = 100;
  const albumId = $derived(route.albumId ?? '');
  const numeric = $derived(/^\d+$/.test(albumId));

  let list = $state<ListState<TrackSummary>>({
    phase: 'loading',
    items: [],
    next_cursor: null,
    has_more: false,
    loadingMore: false,
    message: null,
    code: null,
    requestId: null,
  });
  let paginator: Paginator<TrackSummary> | null = null;
  // 后端没有单张专辑的详情端点，标题只能借第一首曲目的详情取回。
  let heading = $state<{ title: string; artist: string } | null>(null);
  let notice = $state<string | null>(null);

  $effect(() => {
    heading = null;
    if (!numeric) {
      list = { ...list, phase: 'error', message: '专辑地址不合法。', code: 'INVALID_REQUEST' };
      return;
    }
    const next = new Paginator<TrackSummary>({
      fetch: (cursor, signal) => listAlbumTracks(albumId, { limit: PAGE, cursor, signal }),
    });
    paginator = next;
    const unsubscribe = next.subscribe((state) => (list = state));
    void next.load().then(() => {
      const first = list.items[0];
      if (!first) return;
      void getTrack(first.id)
        .then((detail) => (heading = { title: detail.album_title, artist: detail.artist }))
        .catch(() => {
          // 标题拿不到不阻断列表：退化成按 ID 显示。
        });
    });
    return () => {
      unsubscribe();
      next.stop();
      if (paginator === next) paginator = null;
    };
  });

  async function playAlbum(): Promise<void> {
    notice = null;
    try {
      const result = await queueIds('album', { albumId });
      player.clearQueue();
      await player.enqueueIds(result.ids, result.truncated);
      if (result.ids.length > 0) await player.playAt(0);
    } catch (caught) {
      notice = caught instanceof Error ? caught.message : '无法载入这张专辑';
    }
  }
</script>

<svelte:head>
  <title>LiteBeat · 专辑详情</title>
</svelte:head>

<div class="page-head">
  <button type="button" class="btn" data-testid="albums-back" onclick={() => navigate('/albums')}>
    ← 全部专辑
  </button>
  <h1 data-testid="album-title">{heading?.title ?? `专辑 #${albumId || '?'}`}</h1>
  <p class="hint-line">{heading?.artist || '未知歌手'}</p>
  {#if numeric}
    <div class="toolbar">
      <button type="button" class="btn primary" data-testid="album-play-all" onclick={() => void playAlbum()}>
        播放这张专辑
      </button>
    </div>
  {/if}
  {#if notice}
    <p class="hint-line" role="status" style="color: var(--danger)">{notice}</p>
  {/if}
</div>

{#if numeric}
  <TrackList
    tracks={list.items}
    phase={list.phase}
    message={list.message}
    code={list.code}
    requestId={list.requestId}
    hasMore={list.has_more}
    loadingMore={list.loadingMore}
    emptyText="这张专辑里没有可播放的曲目。"
    onMore={() => void paginator?.loadMore()}
    onRetry={() => void paginator?.load()}
  />
{:else}
  <p class="state" role="alert">{list.message}</p>
{/if}

<style>
  .state {
    color: #aab6c2;
    background: #192129;
    border-radius: 10px;
    padding: 14px 16px;
    font-size: 0.95rem;
  }
</style>
