<script lang="ts">
  import type { AlbumSummary } from '../contracts';
  import type { ListPhase } from '../pagination';
  import { navigate } from '../router';

  // 没有封面图可依赖：自托管库里缩略图要到 T12 才有，这里先用文字块占位。
  let {
    albums,
    phase = 'ready',
    message = null,
    code = null,
    requestId = null,
    hasMore = false,
    loadingMore = false,
    onMore,
    onRetry,
  }: {
    albums: AlbumSummary[];
    phase?: ListPhase;
    message?: string | null;
    code?: string | null;
    requestId?: string | null;
    hasMore?: boolean;
    loadingMore?: boolean;
    onMore?: () => void;
    onRetry?: () => void;
  } = $props();

  function initial(title: string): string {
    const first = Array.from(title.trim())[0];
    return first ? first.toUpperCase() : '?';
  }
</script>

{#if phase === 'loading'}
  <p class="state">正在读取专辑…</p>
{:else if phase === 'error'}
  <div class="state error" role="alert">
    <p>{message ?? '专辑列表加载失败'}</p>
    {#if code}<p class="code">{code}{requestId ? ` · 请求号 ${requestId}` : ''}</p>{/if}
    {#if onRetry}<button type="button" onclick={() => onRetry()}>重新加载</button>{/if}
  </div>
{:else if albums.length === 0}
  <p class="state" data-testid="album-empty">还没有按目录聚合出的专辑。扫描带专辑标签的曲目后即可看到。</p>
{:else}
  <ul class="grid" data-testid="album-grid">
    {#each albums as album (album.id)}
      <li>
        <button
          type="button"
          class="card"
          data-testid="album-card"
          onclick={() => navigate(`/albums/${encodeURIComponent(album.id)}`)}
        >
          <span class="art" aria-hidden="true">{initial(album.title)}</span>
          <span class="title">{album.title}</span>
          <span class="sub">{album.album_artist || '未知歌手'}</span>
        </button>
      </li>
    {/each}
  </ul>
  {#if hasMore}
    <div class="more">
      <button type="button" data-testid="album-more" disabled={loadingMore} onclick={() => onMore?.()}>
        {loadingMore ? '加载中…' : '加载更多专辑'}
      </button>
    </div>
  {/if}
{/if}

<style>
  .state {
    color: #aab6c2;
    background: #192129;
    border-radius: 10px;
    padding: 14px 16px;
    font-size: 0.95rem;
  }

  .error {
    background: #2a1a1a;
    border: 1px solid #5a2b2b;
    color: #f3f6f8;
  }

  .error .code {
    color: #aab6c2;
    font-size: 0.8rem;
  }

  .error button,
  .more button {
    background: #42d3ad;
    color: #101418;
    border: 0;
    border-radius: 8px;
    padding: 9px 14px;
    font-size: 0.9rem;
    font-weight: 600;
    cursor: pointer;
  }

  .grid {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(140px, 1fr));
    gap: 12px;
  }

  .card {
    display: flex;
    flex-direction: column;
    gap: 6px;
    width: 100%;
    text-align: left;
    background: #192129;
    border: 1px solid #2b3743;
    border-radius: 12px;
    padding: 10px;
    cursor: pointer;
    box-sizing: border-box;
  }

  .art {
    display: grid;
    place-items: center;
    aspect-ratio: 1;
    border-radius: 8px;
    background: #223040;
    color: #42d3ad;
    font-size: 2rem;
    font-weight: 700;
  }

  .title {
    color: #f3f6f8;
    font-size: 0.92rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .sub {
    color: #aab6c2;
    font-size: 0.78rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .more {
    display: flex;
    justify-content: center;
    padding: 16px 0 4px;
  }
</style>
