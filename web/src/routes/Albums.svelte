<script lang="ts">
  import AlbumGrid from '../lib/components/AlbumGrid.svelte';
  import { listAlbums } from '../lib/api';
  import type { AlbumSummary } from '../lib/contracts';
  import { Paginator, type ListState } from '../lib/pagination';

  const PAGE = 24;
  let list = $state<ListState<AlbumSummary>>({
    phase: 'loading',
    items: [],
    next_cursor: null,
    has_more: false,
    loadingMore: false,
    message: null,
    code: null,
    requestId: null,
  });
  let paginator: Paginator<AlbumSummary> | null = null;

  $effect(() => {
    const next = new Paginator<AlbumSummary>({
      fetch: (cursor, signal) => listAlbums({ limit: PAGE, cursor, signal }),
    });
    paginator = next;
    const unsubscribe = next.subscribe((state) => (list = state));
    void next.load();
    return () => {
      unsubscribe();
      next.stop();
      if (paginator === next) paginator = null;
    };
  });
</script>

<svelte:head>
  <title>LiteBeat · 专辑</title>
</svelte:head>

<div class="page-head">
  <h1>专辑</h1>
  <p class="hint-line">按扫描时的目录聚合；没有专辑标签的曲目不会出现在这里。</p>
</div>

<AlbumGrid
  albums={list.items}
  phase={list.phase}
  message={list.message}
  code={list.code}
  requestId={list.requestId}
  hasMore={list.has_more}
  loadingMore={list.loadingMore}
  onMore={() => void paginator?.loadMore()}
  onRetry={() => void paginator?.load()}
/>
