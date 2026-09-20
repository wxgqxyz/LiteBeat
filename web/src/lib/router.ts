// 极简 History 路由：只负责「当前该渲染哪个页面」，页面内容渲染在 AppShell 内部，
// 因此切换路由不会卸载根布局里唯一的 <audio>。

export type RouteName = 'home' | 'library' | 'albums' | 'album' | 'search' | 'login' | 'unknown';

export interface Route {
  name: RouteName;
  path: string;
  query: URLSearchParams;
  albumId: string | null;
}

type Listener = (route: Route) => void;

const listeners = new Set<Listener>();
let current: Route = { name: 'home', path: '/', query: new URLSearchParams(), albumId: null };
let started = false;

export function parseRoute(fullPath: string): Route {
  const [rawPath, rawQuery = ''] = fullPath.split('?');
  const segments = rawPath.split('/').filter(Boolean);
  const query = new URLSearchParams(rawQuery);
  const base: Omit<Route, 'name' | 'albumId'> = { path: `/${segments.join('/')}`, query };
  if (segments.length === 0) return { ...base, name: 'home', albumId: null };
  switch (segments[0]) {
    case 'library':
      return { ...base, name: 'library', albumId: null };
    case 'albums':
      return segments[1]
        ? { ...base, name: 'album', albumId: segments[1] }
        : { ...base, name: 'albums', albumId: null };
    case 'search':
      return { ...base, name: 'search', albumId: null };
    case 'login':
      return { ...base, name: 'login', albumId: null };
    default:
      return { ...base, name: 'unknown', albumId: null };
  }
}

function readLocation(): Route {
  return parseRoute(`${window.location.pathname}${window.location.search}`);
}

export function currentRoute(): Route {
  return current;
}

export function subscribeRoute(listener: Listener): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

function commit(route: Route): void {
  current = route;
  for (const listener of [...listeners]) listener(current);
}

function onPopState(): void {
  commit(readLocation());
}

export function startRouter(): Route {
  if (!started && typeof window !== 'undefined') {
    window.addEventListener('popstate', onPopState);
    started = true;
  }
  current = readLocation();
  return current;
}

export function stopRouter(): void {
  if (started && typeof window !== 'undefined') {
    window.removeEventListener('popstate', onPopState);
    started = false;
  }
}

export function navigate(path: string, options: { replace?: boolean } = {}): void {
  if (typeof window === 'undefined') return;
  const route = parseRoute(path);
  if (route.path === current.path && route.query.toString() === current.query.toString()) return;
  const url = route.query.toString() ? `${route.path}?${route.query}` : route.path;
  if (options.replace) window.history.replaceState({}, '', url);
  else window.history.pushState({}, '', url);
  commit(route);
}

// 站内链接统一走这里：修饰键点击仍交给浏览器开新标签。
export function followLink(event: MouseEvent | KeyboardEvent, path: string): void {
  if ('button' in event && event.button !== 0) return;
  if (event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
  event.preventDefault();
  navigate(path);
}

export function artistQuery(artist: string): string {
  const query = new URLSearchParams();
  if (artist.trim()) query.set('artist', artist.trim());
  const text = query.toString();
  return text ? `/library?${text}` : '/library';
}
