/** No credentials: transient UI scope used to fence a different account/authorization. */
export function sdkAuthorityScope(user: { user_id: string; tenant_id?: string; permissions: string[]; roles: string[]; tier: string } | null): string {
  const scope: unknown[] = [user?.user_id, user?.permissions, user?.roles, user?.tier];
  if (user?.tenant_id !== undefined) scope.push(user.tenant_id);
  return JSON.stringify(scope);
}
