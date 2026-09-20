<script lang="ts">
  import { untrack } from 'svelte';
  import SearchBox from '../lib/components/SearchBox.svelte';
  import TrackList from '../lib/components/TrackList.svelte';
  import { SearchController } from '../lib/search';
  import { navigate, type Route } from '../lib/router';
  import { player } from '../lib/player/state';

  let { route }: { route: Route } = $props();

  const controller = new SearchController({ limit: 20, debounceMs: 300 });
  let state = $state(controller.snapshot);

  // 只在挂载时读一次地址栏：之后由输入框驱动，避免 URL 回写又触发重搜。
  $effect(() => {
    const initial = untrack(() => route.query.get('q')?.trim() ?? '');
    const unsubscribe = controller.subscribe((next) => (state = next));
    if (initial) {
      controller.setText(initial);
      controller.submit();
    }
    return unsubscribe;
  });

  $effect(() => {
    const query = state.query;
    if (!query) return;
    const search = new URLSearchParams({ q: query });
    navigate(`/search?${search}`, { replace: true });
  });

  async function playResults(): Promise<void> {
    // 搜索结果没有稳定的 queue-ids 范围，只能把已加载的这些首入队。
    player.clearQueue();
    for (const track of state.items) await player.enqueue(track);
    if (state.items.length > 0) await player.playAt(0);
  }

  const listPhase = $derived.by(() => {
    switch (state.phase) {
      case 'loading':
        return 'loading' as const;
      case 'ready':
        return 'ready' as const;
      case 'error':
      case 'too-long':
        return 'error' as const;
      default:
        return 'empty' as const;
    }
  });
</script>

<svelte:head>
  <title>LiteBeat · 搜索</title>
</svelte:head>

<div class="page-head">
  <h1>搜索曲库</h1>
  <SearchBox {controller} />
  <p class="hint-line" data-testid="search-count">
    {#if state.query}
      「{state.query}」命中 {state.items.length} 首{state.has_more ? '（还有更多）' : ''}
    {:else}
      支持中文、全角与大小写不敏感；百分号、下划线、引号都按字面匹配。
    {/if}
  </p>
  {#if state.items.length > 0}
    <div class="toolbar">
      <button type="button" class="btn primary" data-testid="search-play" onclick={() => void playResults()}>
        播放这些结果
      </button>
    </div>
  {/if}
</div>

<TrackList
  tracks={state.items}
  phase={listPhase}
  message={state.message ?? (state.phase === 'blank' ? '输入关键词后开始搜索。' : null)}
  hasMore={state.has_more}
  loadingMore={state.loadingMore}
  emptyText={state.query ? '没有匹配的曲目。' : '还没有搜索。'}
  onMore={() => void controller.loadMore()}
  onRetry={() => controller.submit()}
/>
