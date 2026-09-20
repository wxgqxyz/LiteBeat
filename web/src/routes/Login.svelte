<script lang="ts">
  import { ApiError, login } from '../lib/api';

  let username = $state('');
  let password = $state('');
  let error = $state('');
  let busy = $state(false);

  // 只接受站内相对路径，避免把用户送到外站的开放重定向。
  function nextLocation(): string {
    const next = new URL(window.location.href).searchParams.get('next');
    if (!next || !next.startsWith('/') || next.startsWith('//')) return '/';
    return next;
  }

  async function handleSubmit(event: SubmitEvent) {
    event.preventDefault();
    if (busy) return;
    busy = true;
    error = '';
    try {
      await login(username.trim(), password);
      window.location.assign(nextLocation());
      return;
    } catch (caught) {
      error = caught instanceof ApiError ? caught.message : '登录失败，请稍后重试';
    } finally {
      busy = false;
    }
    // 失败时保留账号、清除口令，方便重输。
    password = '';
  }
</script>

<svelte:head>
  <title>LiteBeat · 管理员登录</title>
</svelte:head>

<main class="login">
  <h1>管理员登录</h1>
  <p class="hint">登录后即可访问曲库、播放与歌单。</p>

  <form onsubmit={handleSubmit}>
    <label>
      <span>用户名</span>
      <input name="username" autocomplete="username" maxlength="64" required bind:value={username} />
    </label>
    <label>
      <span>口令</span>
      <input
        name="password"
        type="password"
        autocomplete="current-password"
        maxlength="128"
        required
        bind:value={password}
      />
    </label>

    {#if error}
      <p class="error" role="alert" aria-live="polite">{error}</p>
    {/if}

    <button type="submit" disabled={busy || username.trim() === '' || password === ''}>
      {busy ? '登录中…' : '登录'}
    </button>
  </form>
</main>

<style>
  .login {
    display: flex;
    flex-direction: column;
    gap: 8px;
    max-width: 420px;
    padding: 16vh 24px 24px;
  }

  h1 {
    font-size: 1.75rem;
    margin: 0;
    color: #42d3ad;
  }

  .hint {
    margin: 0 0 12px;
    color: #aab6c2;
    font-size: 0.95rem;
  }

  form {
    display: flex;
    flex-direction: column;
    gap: 14px;
  }

  label {
    display: flex;
    flex-direction: column;
    gap: 6px;
    color: #aab6c2;
    font-size: 0.9rem;
  }

  input {
    background: #192129;
    color: #f3f6f8;
    border: 1px solid #2b3743;
    border-radius: 8px;
    padding: 10px 12px;
    font-size: 1rem;
  }

  input:focus-visible {
    outline: 2px solid #42d3ad;
    outline-offset: 1px;
  }

  button {
    background: #42d3ad;
    color: #101418;
    border: 0;
    border-radius: 8px;
    padding: 10px 14px;
    font-size: 1rem;
    font-weight: 600;
    cursor: pointer;
  }

  button:disabled {
    background: #2b3743;
    color: #aab6c2;
    cursor: not-allowed;
  }

  .error {
    margin: 0;
    color: var(--danger);
    background: #192129;
    border-radius: 8px;
    padding: 10px 14px;
    font-size: 0.95rem;
  }
</style>
