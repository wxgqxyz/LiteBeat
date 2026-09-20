<script lang="ts">
  import type { Snippet } from 'svelte';
  import { player, type StoreSnapshot } from '../player/state';
  import { currentRoute, followLink, subscribeRoute, type Route } from '../router';
  import PlayerBar from './PlayerBar.svelte';
  import PlayerSheet from './PlayerSheet.svelte';
  import QueueDrawer from './QueueDrawer.svelte';
  import StatusPanel from './StatusPanel.svelte';

  // 音频元素只在这里渲染一次：路由切换只替换 main 里的页面，引擎始终挂在同一个元素上。
  let {
    username,
    onSignOut,
    children,
  }: { username: string; onSignOut?: () => void; children: Snippet } = $props();

  let audioEl = $state<HTMLAudioElement | null>(null);
  let snapshot = $state<StoreSnapshot>(player.snapshot);
  let route = $state<Route>(currentRoute());
  let panel = $state<'none' | 'queue' | 'status'>('none');
  let sheetOpen = $state(false);

  $effect(() => {
    if (!audioEl) return;
    player.attachAudio(audioEl);
    // 恢复队列/位置必须有音频元素：引擎在元素未挂载时会落到 error。
    void player.restore();
    return () => player.attachAudio(null);
  });
  $effect(() => player.subscribe((next) => (snapshot = next)));
  $effect(() => subscribeRoute((next) => (route = next)));
  $effect(() => {
    void route;
    panel = 'none';
  });

  const active = (...names: Route['name'][]) => names.includes(route.name);

  function go(event: MouseEvent, path: string): void {
    followLink(event, path);
    if (event.currentTarget instanceof HTMLAnchorElement) event.currentTarget.blur();
  }
</script>

<svelte:window
  onkeydown={(event) => {
    if (event.key === 'Escape') {
      panel = 'none';
      sheetOpen = false;
    }
  }}
/>

<div class="shell" class:has-bar={snapshot.player.track !== null}>
  <header class="topbar">
    <a class="brand" href="/" data-testid="nav-home" onclick={(e) => go(e, '/')}>LiteBeat</a>
    <nav aria-label="主导航">
      <a href="/library" data-testid="nav-library" class:on={active('library', 'home')}
        onclick={(e) => { e.preventDefault(); go(e, '/library'); }}>音乐库</a>
      <a href="/albums" data-testid="nav-albums" class:on={active('albums', 'album')}
        onclick={(e) => { e.preventDefault(); go(e, '/albums'); }}>专辑</a>
      <a href="/search" data-testid="nav-search" class:on={active('search')}
        onclick={(e) => { e.preventDefault(); go(e, '/search'); }}>搜索</a>
    </nav>
    <div class="tools">
      <button
        type="button"
        data-testid="queue-open"
        class={{ on: panel === 'queue', flash: snapshot.queue.size > 0 }}
        onclick={() => (panel = panel === 'queue' ? 'none' : 'queue')}
      >
        队列 {#if snapshot.queue.size > 0}<span>{snapshot.queue.size}</span>{/if}
      </button>
      <button
        type="button"
        data-testid="status-open"
        class:on={panel === 'status'}
        onclick={() => (panel = panel === 'status' ? 'none' : 'status')}
      >
        状态
      </button>
      <span class="who">{username}</span>
      {#if onSignOut}
        <button type="button" data-testid="sign-out" onclick={() => onSignOut()}>退出</button>
      {/if}
    </div>
  </header>

  {#if panel !== 'none'}
    {#if panel === 'queue'}
      <QueueDrawer {snapshot} onClose={() => (panel = 'none')} />
    {:else}
      <StatusPanel {username} {snapshot} onClose={() => (panel = 'none')} />
    {/if}
  {/if}

  <main class="page">
    {@render children()}
  </main>

  {#if sheetOpen && snapshot.player.track}
    <PlayerSheet {snapshot} onClose={() => (sheetOpen = false)} />
  {/if}
  <PlayerBar {snapshot} onExpand={() => (sheetOpen = !sheetOpen)} />
</div>

<audio bind:this={audioEl} data-testid="audio" preload="none"></audio>

<style>
  .shell {
    display: flex;
    flex-direction: column;
    min-height: 100vh;
    box-sizing: border-box;
  }

  .shell.has-bar .page {
    padding-bottom: 104px;
  }

  .page {
    flex: 1;
    width: 100%;
    max-width: 1100px;
    margin: 0 auto;
    padding: 16px;
    box-sizing: border-box;
  }

  .topbar {
    position: sticky;
    top: 0;
    z-index: 30;
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 10px 16px;
    background: #141a20;
    border-bottom: 1px solid #2b3743;
    flex-wrap: wrap;
  }

  .brand {
    color: #42d3ad;
    font-weight: 700;
    font-size: 1.15rem;
    text-decoration: none;
    letter-spacing: 0.02em;
  }

  nav {
    display: flex;
    gap: 4px;
    flex: 1;
  }

  nav a {
    color: #aab6c2;
    text-decoration: none;
    padding: 8px 12px;
    border-radius: 8px;
    font-size: 0.95rem;
  }

  nav a.on {
    color: #f3f6f8;
    background: #192129;
  }

  .tools {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .tools button {
    background: transparent;
    color: #aab6c2;
    border: 1px solid #2b3743;
    border-radius: 8px;
    padding: 7px 10px;
    font-size: 0.85rem;
    cursor: pointer;
  }

  .tools button.on,
  .tools button.flash {
    color: #101418;
    background: #42d3ad;
    border-color: #42d3ad;
  }

  .tools button span {
    font-variant-numeric: tabular-nums;
  }

  .who {
    color: #aab6c2;
    font-size: 0.8rem;
  }

  @media (min-width: 720px) {
    .page {
      padding: 24px;
    }
  }
</style>
