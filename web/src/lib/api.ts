export class ApiError extends Error {
  constructor(
    public status: number,
    message: string,
  ) {
    super(message);
  }
  get isNetwork() {
    return this.status === 0;
  }
}

let token: string | null = null;
let reauth: (() => Promise<void>) | null = null;

export function setToken(t: string | null) {
  token = t;
}
export function onReauth(fn: () => Promise<void>) {
  reauth = fn;
}

async function raw(method: string, path: string, body?: BodyInit, headers: Record<string, string> = {}): Promise<Response> {
  const h: Record<string, string> = { ...headers };
  if (token) h['Authorization'] = `Bearer ${token}`;
  try {
    return await fetch(`/api${path}`, { method, body, headers: h });
  } catch {
    throw new ApiError(0, 'network');
  }
}

async function request<T>(method: string, path: string, json?: unknown, retry = true): Promise<T> {
  const body = json === undefined ? undefined : JSON.stringify(json);
  const res = await raw(method, path, body, body ? { 'Content-Type': 'application/json' } : {});
  if (res.status === 401 && retry && reauth && path !== '/auth/telegram') {
    // сессия истекла — один раз входим заново по свежему initData и повторяем запрос
    await reauth();
    return request<T>(method, path, json, false);
  }
  const data = await res.json().catch(() => ({}));
  if (!res.ok) throw new ApiError(res.status, (data as { error?: string }).error ?? 'error');
  return data as T;
}

export const api = {
  get: <T>(path: string) => request<T>('GET', path),
  post: <T>(path: string, body: unknown = {}) => request<T>('POST', path, body),
  put: <T>(path: string, body: unknown) => request<T>('PUT', path, body),
  async upload<T>(path: string, blob: Blob): Promise<T> {
    const res = await raw('POST', path, blob, { 'Content-Type': 'image/jpeg' });
    const data = await res.json().catch(() => ({}));
    if (!res.ok) throw new ApiError(res.status, (data as { error?: string }).error ?? 'error');
    return data as T;
  },
  /** Фото закрыто авторизацией, поэтому <img src> не подходит: берём байты с токеном и отдаём blob-URL. */
  async photoUrl(path: string): Promise<string | null> {
    const res = await raw('GET', path);
    if (!res.ok) return null;
    return URL.createObjectURL(await res.blob());
  },
};

export function errorText(e: unknown, fallback: string, offline: string): string {
  if (e instanceof ApiError) return e.isNetwork ? offline : e.message;
  return fallback;
}
