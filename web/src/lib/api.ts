import type { Id } from './contracts';

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
  const text = await response.text();
  const body: unknown = text ? JSON.parse(text) : null;
  if (!response.ok) {
    if (response.status === 401) clearSessionCache();
    throw toApiError(response.status, body);
  }
  return body;
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
