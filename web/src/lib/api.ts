import type {
  AlbumSummary,
  Id,
  Page,
  QueueIds,
  TrackDetail,
  TrackSummary,
} from './contracts';

export interface SessionInfo {
  user_id: Id;
  username: string;
  csrf_token: string;
}

interface ErrorBody {
  error: { code: string; message: string; request_id: string };
}

export class ApiError extends Error {
  readonly status: number;
  readonly code: string;
  readonly requestId: string;

  constructor(status: number, code: string, message: string, requestId: string) {
    super(message);
    this.name = 'ApiError';
    this.status = status;
    this.code = code;
    this.requestId = requestId;
  }
}

const API_ROOT = '/api/v1';

// 会话里的 CSRF 令牌缓存在模块内：修改类请求自动带上，登录成功后才有效。
let csrfToken: string | null = null;

export function cachedCsrfToken(): string | null {
  return csrfToken;
}

// 退出或会话失效时清空受保护状态；音频资源由播放器引擎负责释放。
export function clearSessionCache(): void {
  csrfToken = null;
}

function isSessionInfo(value: unknown): value is SessionInfo {
  if (typeof value !== 'object' || value === null) return false;
  const record = value as Record<string, unknown>;
  return (
    typeof record.user_id === 'string' &&
    typeof record.username === 'string' &&
    typeof record.csrf_token === 'string'
  );
}

function toApiError(status: number, body: unknown): ApiError {
  const error = (body as ErrorBody | null)?.error;
  if (error && typeof error.code === 'string') {
    return new ApiError(status, error.code, error.message, error.request_id);
  }
  return new ApiError(status, 'INTERNAL', `请求失败（HTTP ${status}）`, '');
}

async function send(path: string, init: RequestInit): Promise<unknown> {
  const method = (init.method ?? 'GET').toUpperCase();
  const headers = new Headers(init.headers);
  headers.set('accept', 'application/json');
  // 修改类请求带 CSRF 头；登录时尚无会话，因此不会带上。
  if (method !== 'GET' && method !== 'HEAD' && csrfToken && !headers.has('x-csrf-token')) {
    headers.set('x-csrf-token', csrfToken);
  }
  const response = await fetch(`${API_ROOT}${path}`, {
    ...init,
    method,
    headers,
    credentials: 'same-origin',
  });
  if (response.status === 401) clearSessionCache();
  if (!response.ok) {
    const body: unknown = await readBody(response);
    const error = toApiError(response.status, body);
    recordError(error);
    throw error;
  }
  return readBody(response);
}

async function readBody(response: Response): Promise<unknown> {
  const text = await response.text();
  if (!text) return null;
  // 反向代理可能给出 HTML 错误页：解析失败时按空体处理，由状态码决定错误。
  try {
    return JSON.parse(text) as unknown;
  } catch {
    return null;
  }
}

export async function login(username: string, password: string): Promise<void> {
  clearSessionCache();
  await send('/auth/login', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ username, password }),
  });
}

export async function getSession(): Promise<SessionInfo> {
  const body = await send('/auth/session', { method: 'GET' });
  if (!isSessionInfo(body)) {
    throw new ApiError(500, 'INTERNAL', '会话响应格式不合法', '');
  }
  csrfToken = body.csrf_token;
  return body;
}

export async function logout(): Promise<void> {
  const token = csrfToken ?? (await getSession()).csrf_token;
  await send('/auth/logout', { method: 'POST', headers: { 'x-csrf-token': token } });
  clearSessionCache();
}

// 最近一次请求失败：StatusPanel 需要如实展示 error.code 与 request_id。
let lastError: ApiError | null = null;
const errorListeners = new Set<(error: ApiError | null) => void>();

function recordError(error: ApiError): void {
  lastError = error;
  for (const listener of errorListeners) listener(error);
}

export function lastApiError(): ApiError | null {
  return lastError;
}

export function clearApiError(): void {
  lastError = null;
  for (const listener of errorListeners) listener(null);
}

export function subscribeApiError(listener: (error: ApiError | null) => void): () => void {
  errorListeners.add(listener);
  return () => errorListeners.delete(listener);
}

function isBudgetError(error: unknown): boolean {
  return (
    error instanceof ApiError &&
    error.status === 503 &&
    (error.code === 'QUERY_TIMEOUT' || error.code === 'LIBRARY_BUSY')
  );
}

const wait = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));

// 只读查询有 200ms 预算，到点返回 503；这里只做一次有界重试，不循环打库。
export async function getJson<T>(
  path: string,
  init: { signal?: AbortSignal } = {},
): Promise<T> {
  try {
    return (await send(path, { method: 'GET', signal: init.signal })) as T;
  } catch (error) {
    if (!isBudgetError(error) || init.signal?.aborted) throw error;
    await wait(250);
    if (init.signal?.aborted) throw error;
    return (await send(path, { method: 'GET', signal: init.signal })) as T;
  }
}

export type TrackSort = 'title' | 'recent';

export interface ListOptions {
  limit?: number;
  cursor?: string | null;
  signal?: AbortSignal;
}

function query(params: Record<string, string | number | undefined | null>): string {
  const search = new URLSearchParams();
  for (const [key, value] of Object.entries(params)) {
    if (value !== undefined && value !== null && value !== '') search.set(key, String(value));
  }
  const text = search.toString();
  return text ? `?${text}` : '';
}

export function listTracks(
  options: ListOptions & { artist?: string; sort?: TrackSort } = {},
): Promise<Page<TrackSummary>> {
  return getJson(
    `/tracks${query({
      limit: options.limit,
      cursor: options.cursor,
      artist: options.artist,
      sort: options.sort,
    })}`,
    { signal: options.signal },
  );
}

export function getTrack(id: Id, options: { signal?: AbortSignal } = {}): Promise<TrackDetail> {
  return getJson(`/tracks/${encodeURIComponent(id)}`, { signal: options.signal });
}

export function listAlbums(options: ListOptions = {}): Promise<Page<AlbumSummary>> {
  return getJson(`/albums${query({ limit: options.limit, cursor: options.cursor })}`, {
    signal: options.signal,
  });
}

export function listAlbumTracks(
  albumId: Id,
  options: ListOptions = {},
): Promise<Page<TrackSummary>> {
  return getJson(
    `/albums/${encodeURIComponent(albumId)}/tracks${query({
      limit: options.limit,
      cursor: options.cursor,
    })}`,
    { signal: options.signal },
  );
}

export function searchTracks(
  q: string,
  options: ListOptions = {},
): Promise<Page<TrackSummary>> {
  return getJson(
    `/search${query({ q, limit: options.limit, cursor: options.cursor })}`,
    { signal: options.signal },
  );
}

export type QueueScope = 'library' | 'album' | 'favorites' | 'playlist';

export function queueIds(
  scope: QueueScope,
  options: { albumId?: Id; playlistId?: Id; signal?: AbortSignal } = {},
): Promise<QueueIds> {
  return getJson(
    `/queue-ids${query({
      scope,
      album_id: options.albumId,
      playlist_id: options.playlistId,
    })}`,
    { signal: options.signal },
  );
}
