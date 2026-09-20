<script lang="ts">
  import { clearApiError, lastApiError, subscribeApiError } from '../api';
  import { player } from '../player/state';
  import type { StoreSnapshot } from '../player/state';

  // 状态面板把后端错误原样摊开：code 与 request_id 是排查自托管实例的唯一线索。
  let {
    username,
    snapshot,
    onClose,
  }: { username: string; snapshot: StoreSnapshot; onClose: () => void } = $props();

  let health = $state('检查中…');
  let error = $state(lastApiError());

  $effect(() => subscribeApiError((next) => (error = next)));

  async function probe(): Promise<void> {
    try {
      const response = await fetch('/health/live', { headers: { accept: 'application/json' } });
      health = response.ok ? '服务正常' : `服务异常（HTTP ${response.status}）`;
    } catch {
      health = '无法连接服务';
    }
  }
  $effect(() => {
    void probe();
  });
</script>

<aside class="panel" data-testid="status-panel" aria-label="运行状态">
  <div class="head">
    <strong>运行状态</strong>
    <button type="button" onclick={onClose} aria-label="关闭状态面板">关闭</button>
  </div>

  <dl>
    <dt>服务</dt>
    <dd>{health}</dd>
    <dt>账号</dt>
    <dd>{username}</dd>
    <dt>播放状态</dt>
    <dd data-testid="status-player-state">{snapshot.player.state}</dd>
    <dt>队列</dt>
    <dd>{snapshot.queue.size} / {snapshot.queue.limit} 首</dd>
    <dt>待补详情</dt>
    <dd>{snapshot.queue.missing} 首</dd>
  </dl>

  {#if snapshot.notice}
    <p class="notice" data-testid="status-notice">{snapshot.notice}</p>
    <button type="button" class="ghost" onclick={() => player.setNotice(null)}>清除提示</button>
  {/if}

  {#if error}
    <div class="error" data-testid="status-error">
      <p><strong>{error.code}</strong> · {error.message}</p>
      <p class="req">请求号 {error.requestId || '（无）'} · HTTP {error.status}</p>
    </div>
    <button type="button" class="ghost" onclick={() => clearApiError()}>清除错误</button>
  {:else}
    <p class="hint">最近没有失败的请求。</p>
  {/if}
</aside>

<style>
  .panel {
    position: fixed;
    top: 56px;
    right: 8px;
    left: 8px;
    z-index: 40;
    max-height: 70vh;
    overflow: auto;
    background: #141a20;
    border: 1px solid #2b3743;
    border-radius: 12px;
    padding: 14px 16px;
    box-sizing: border-box;
  }

  .head {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 8px;
  }

  .head button,
  .ghost {
    background: transparent;
    color: #aab6c2;
    border: 1px solid #2b3743;
    border-radius: 8px;
    padding: 6px 10px;
    font-size: 0.8rem;
    cursor: pointer;
  }

  dl {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 6px 12px;
    margin: 0 0 12px;
    font-size: 0.9rem;
  }

  dt {
    color: #aab6c2;
  }

  dd {
    margin: 0;
    color: #f3f6f8;
  }

  .notice,
  .error p,
  .hint {
    margin: 6px 0;
    font-size: 0.85rem;
    color: #f3f6f8;
  }

  .notice {
    background: #192129;
    border-radius: 8px;
    padding: 8px 10px;
  }

  .error {
    background: #2a1a1a;
    border: 1px solid #5a2b2b;
    border-radius: 8px;
    padding: 8px 10px;
  }

  .error .req {
    color: #aab6c2;
    font-size: 0.78rem;
  }

  .hint {
    color: #aab6c2;
  }

  @media (min-width: 720px) {
    .panel {
      left: auto;
      width: 360px;
    }
  }
</style>
