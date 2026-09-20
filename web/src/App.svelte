<script lang="ts">
  import { ApiError, getSession, logout } from './lib/api';
  import AppShell from './lib/components/AppShell.svelte';
  import { navigate, startRouter, stopRouter, subscribeRoute, type Route } from './lib/router';
  import { player } from './lib/player/state';
  import AlbumDetail from './routes/AlbumDetail.svelte';
  import Albums from './routes/Albums.svelte';
  import Home from './routes/Home.svelte';
  import Library from './routes/Library.svelte';
  import Login from './routes/Login.svelte';
  import Search from './routes/Search.svelte';

  // 会话闸门在 App，页面在 AppShell 之内：外壳持有唯一的音频元素，
  // 因此切换页面不会重建它，播放才能跨过路由。
  let route = $state<Route>(startRouter());
  let username = $state('');
  let gate = $state<'checking' | 'anonymous' | 'ready' | 'error'>('checking');
  let detail = $state('');

  $effect(() => {
    const unsubscribe = subscribeRoute((next) => (route = next));
    void boot();
    return () => {
      unsubscribe();
      stopRouter();
    };
  });

  function nextParam(): string {
    return encodeURIComponent(`${window.location.pathname}${window.location.search}`);
  }

  async function boot(): Promise<void> {
    try {
      const session = await getSession();
      username = session.username;
      gate = 'ready';
      detail = '';
      if (route.name === 'login') navigate('/', { replace: true });
    } catch (caught) {
      if (caught instanceof ApiError && caught.status === 401) {
        gate = 'anonymous';
        if (route.name !== 'login') navigate(`/login?next=${nextParam()}`, { replace: true });
        return;
      }
      gate = 'error';
      detail = caught instanceof Error ? caught.message : '无法读取会话';
    }
  }

  async function signOut(): Promise<void> {
    try {
      await logout();
    } catch {
      // 服务端会话可能已过期，本地状态照样清干净。
    }
    await player.signOut();
    gate = 'anonymous';
    navigate('/login');
  }
</script>

{#if gate === 'ready'}
  <AppShell {username} onSignOut={() => void signOut()}>
    {#if route.name === 'library'}
      <Library {route} />
    {:else if route.name === 'albums'}
      <Albums />
    {:else if route.name === 'album'}
      <AlbumDetail {route} />
    {:else if route.name === 'search'}
      <Search {route} />
    {:else if route.name === 'home'}
      <Home />
    {:else}
      <section class="missing">
        <h1>没有这个页面</h1>
        <p>地址 <code>{route.path}</code> 不属于曲库、专辑或搜索。</p>
        <button type="button" class="btn" data-testid="nav-back-home" onclick={() => navigate('/')}>
          回首页
        </button>
      </section>
    {/if}
  </AppShell>
{:else if gate === 'anonymous'}
  <Login />
{:else if gate === 'error'}
  <main class="gate">
    <h1>会话读取失败</h1>
    <p>{detail}</p>
    <button
      type="button"
      class="btn"
      onclick={() => {
        gate = 'checking';
        void boot();
      }}
    >重试</button>
  </main>
{:else}
  <main class="gate">
    <p class="eyebrow">LITEBEAT</p>
    <h1>正在确认会话…</h1>
  </main>
{/if}

<style>
  .gate,
  .missing {
    max-width: 520px;
    margin: 0 auto;
    padding: 18vh 20px 20px;
  }

  .missing {
    padding: 40px 0;
  }

  .missing h1,
  .gate h1 {
    color: #f3f6f8;
  }

  code {
    background: #192129;
    border-radius: 6px;
    padding: 2px 6px;
    color: #aab6c2;
    font-size: 0.85em;
  }
</style>
