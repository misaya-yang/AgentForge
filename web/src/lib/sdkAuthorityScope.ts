/** No credentials: transient UI scope used to fence a different account/authorization. */
export function sdkAuthorityScope(user: { user_id: string; permissions: string[]; roles: string[]; tier: string } | null): string {
  return JSON.stringify([user?.user_id, user?.permissions, user?.roles, user?.tier]);
}
