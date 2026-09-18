<script lang="ts">
  let status = '检查服务状态…';

  async function checkHealth() {
    try {
      const response = await fetch('/health/live', { headers: { accept: 'application/json' } });
      status = response.ok ? '服务运行正常' : '服务暂不可用';
    } catch {
      status = '无法连接服务';
    }
  }

  checkHealth();
</script>

<svelte:head>
  <meta name="description" content="LiteBeat 自托管音乐库" />
</svelte:head>

<main>
  <header>
    <strong>LiteBeat</strong>
    <span>轻量自托管音乐库</span>
  </header>
  <section aria-labelledby="welcome-title">
    <p class="eyebrow">YOUR MUSIC, YOUR SPACE</p>
    <h1 id="welcome-title">把音乐库留在自己的设备上。</h1>
    <p>扫描本地音乐目录，在浏览器中检索、播放和整理自己的收藏。</p>
    <p class="status" aria-live="polite">{status}</p>
  </section>
</main>
