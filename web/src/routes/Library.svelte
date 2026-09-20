<script lang="ts">
  import TrackList from '../lib/components/TrackList.svelte';
  import { listTracks, type TrackSort } from '../lib/api';
  import type { TrackSummary } from '../lib/contracts';
  import { Paginator, type ListState } from '../lib/pagination';
  import { navigate, type Route } from '../lib/router';
  import { player } from '../lib/player/state';

  let { route }: { route: Route } = $props();

  const PAGE = 50;
  let sort = $state<TrackSort>('title');
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

  // 歌手筛选只从地址栏进来，列表页不做输入框，避免和搜索页职责重叠。
  const artist = $derived(route.query.get('artist')?.trim() ?? '');

  $effect(() => {
    const next = new Paginator<TrackSummary>({
      fetch: (cursor, signal) =>
        listTracks({
          limit: PAGE,
          cursor,
          artist: artist || undefined,
          sort,
          signal,
        }),
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

  function enqueueThisPage(): void {
    for (const track of list.items) void player.enqueue(track);
    player.setNotice(`已把当前页 ${list.items.length} 首加入队列。`);
  }
</script>

<svelte:head>
  <title>LiteBeat · 音乐库</title>
</svelte:head>

<div class="page-head">
  <h1>音乐库</h1>
  <div class="toolbar">
    <div class="seg" role="group" aria-label="排序">
      <button
        type="button"
        data-testid="sort-title"
        class:on={sort === 'title'}
        onclick={() => (sort = 'title')}
      >
        按标题
      </button>
      <button
        type="button"
        data-testid="sort-recent"
        class:on={sort === 'recent'}
        onclick={() => (sort = 'recent')}
      >
        最近入库
      </button>
    </div>
    <button type="button" class="btn" data-testid="enqueue-page" onclick={enqueueThisPage}>
      加入本页
    </button>
    <button type="button" class="btn" onclick={() => navigate('/search')}>去搜索</button>
  </div>

  {#if artist}
    <p class="filter-line" data-testid="artist-filter">
      只看歌手：{artist}
      <button type="button" onclick={() => navigate('/library')}>清除</button>
    </p>
  {/if}
</div>

<TrackList
  tracks={list.items}
  phase={list.phase}
  message={list.message}
  code={list.code}
  requestId={list.requestId}
  hasMore={list.has_more}
  loadingMore={list.loadingMore}
  emptyText={artist ? '这位歌手名下没有曲目。' : '曲库还没有可播放的曲目。'}
  onMore={() => void paginator?.loadMore()}
  onRetry={() => void paginator?.load()}
/>

<p class="hint-line">一页 {PAGE} 首，游标由后端签发，翻页不会重算全库总数。</p>
