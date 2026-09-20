<script lang="ts">
  import { MAX_QUERY_CHARS, type SearchController, type SearchPhase } from '../search';

  // 输入法合成期间不能搜：中文拼音未上屏时发请求只会拿到噪音结果，还会打断候选词。
  let { controller }: { controller: SearchController } = $props();

  let composing = $state(false);
  let text = $state('');
  let phase = $state<SearchPhase>('blank');
  let message = $state<string | null>(null);

  $effect(() =>
    controller.subscribe((next) => {
      // 回填外部改动（例如从地址栏带进来的 q）；用户正在合成时不覆盖候选串。
      if (!composing) text = next.text;
      phase = next.phase;
      message = next.message;
    }),
  );

  function onInput(event: Event): void {
    const value = (event.currentTarget as HTMLInputElement).value;
    text = value;
    if (composing) return;
    controller.setText(value);
  }
</script>

<div class="box">
  <input
    type="search"
    data-testid="search-input"
    class="input"
    placeholder="搜索歌曲或歌手（最多 {MAX_QUERY_CHARS} 字）"
    maxlength={MAX_QUERY_CHARS * 2}
    autocomplete="off"
    aria-label="搜索曲库"
    value={text}
    oncompositionstart={() => {
      composing = true;
      controller.setComposing(true);
    }}
    oncompositionend={(event) => {
      composing = false;
      controller.setComposing(false);
      controller.setText((event.currentTarget as HTMLInputElement).value);
    }}
    oninput={onInput}
    onkeydown={(event) => {
      if (event.key === 'Enter' && !composing) controller.submit();
    }}
  />
  <span class="hint" data-testid="search-hint" aria-live="polite">
    {#if phase === 'composing'}输入法候选中…{:else if phase === 'too-long'}{message ?? ''}{:else if phase === 'loading'}搜索中…{:else if phase === 'error'}{message ?? '搜索失败'}{/if}
  </span>
</div>

<style>
  .box {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .input {
    width: 100%;
    background: #192129;
    color: #f3f6f8;
    border: 1px solid #2b3743;
    border-radius: 10px;
    padding: 12px 14px;
    font-size: 1rem;
  }

  .input:focus-visible {
    outline: 2px solid #42d3ad;
    outline-offset: 1px;
  }

  .hint {
    min-height: 18px;
    color: #aab6c2;
    font-size: 0.8rem;
  }
</style>
