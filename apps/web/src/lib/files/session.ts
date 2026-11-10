export interface Account {
  name: string;
  roles: string[];
}

export type Session =
  | { mode: 'disabled' }
  | { mode: 'development' }
  | { mode: 'oidc'; user?: Account; csrfToken?: string };

function sessionRequest(signal: AbortSignal, timeout = 12_000): RequestInit {
  return {
    cache: 'no-store',
    credentials: 'same-origin',
    mode: 'same-origin',
    redirect: 'error',
    signal: AbortSignal.any([signal, AbortSignal.timeout(timeout)])
  };
}

export async function getSession(signal: AbortSignal): Promise<Session> {
  const response = await fetch('/auth/session', sessionRequest(signal));
  if (!response.ok) throw new Error('Could not check your session. Try again.');
  const data: unknown = await response.json();
  if (data && typeof data === 'object' && 'mode' in data) {
    if (data.mode === 'disabled' || data.mode === 'development') return { mode: data.mode };
    if (data.mode === 'oidc') {
      const user = 'user' in data ? data.user : undefined;
      const csrfToken = 'csrf_token' in data ? data.csrf_token : undefined;
      if (user == null && csrfToken == null) return { mode: 'oidc' };
      if (user && typeof user === 'object' && 'name' in user && typeof user.name === 'string'
        && 'roles' in user && Array.isArray(user.roles)
        && user.roles.every((role: unknown) => typeof role === 'string')
        && typeof csrfToken === 'string' && csrfToken) {
        return { mode: 'oidc', user: { name: user.name, roles: user.roles }, csrfToken };
      }
    }
  }
  throw new Error('The server returned an invalid session. Try again.');
}

export async function endSession(csrfToken: string, signal: AbortSignal): Promise<void> {
  const response = await fetch('/auth/logout', {
    ...sessionRequest(signal, 25_000),
    method: 'POST',
    headers: { 'X-CSRF-Token': csrfToken }
  });
  if (response.status !== 204 && response.status !== 401) {
    throw new Error('Could not sign out. Try again.');
  }
}
