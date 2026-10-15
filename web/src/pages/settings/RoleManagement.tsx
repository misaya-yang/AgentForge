import { useLayoutEffect, useMemo, useRef, useState } from "react";
import { Link } from "react-router-dom";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { useAuthStore } from "@/store/useAuthStore";
import { sdkAuthorityScope } from "@/lib/sdkAuthorityScope";
import { generateUUID } from "@/lib/utils";
import { listRoles, listPermissions, type RoleResponse, type PermissionResponse } from "@/api/users";
import { createRole, readRole, roleMatches, type RoleCreateInput } from "@/api/roles";

type Attempt = { input: RoleCreateInput; accepted: boolean };
function RoleEditor({ isCurrent, canCreate }: { isCurrent: () => boolean; canCreate: boolean }) {
  const { t } = useTranslation();
  const active = useRef(true);
  const working = useRef(false);
  const submitted = useRef<Attempt | null>(null);
  const [roles, setRoles] = useState<RoleResponse[]>([]);
  const [permissions, setPermissions] = useState<PermissionResponse[]>([]);
  const [loaded, setLoaded] = useState(false);
  const [busy, setBusy] = useState(false);
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [selected, setSelected] = useState<string[]>([]);
  const [review, setReview] = useState<RoleCreateInput | null>(null);
  const [attempt, setAttempt] = useState<Attempt | null>(null);
  const [observed, setObserved] = useState<RoleResponse | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const ownsView = () => active.current && isCurrent();
  const refresh = async () => {
    if (!ownsView() || working.current || submitted.current) return;
    working.current = true; setBusy(true); setLoaded(false); setReview(null);
    try {
      const [catalog, codes] = await Promise.all([listRoles(), listPermissions()]);
      if (!ownsView()) return;
      setRoles(catalog.roles); setPermissions(codes.permissions); setLoaded(true); setNotice(null);
      setSelected(previous => previous.filter(code => codes.permissions.some(item => item.permission_code === code)));
    } catch { if (ownsView()) setNotice("roleManagement.readFailed"); }
    finally { working.current = false; if (ownsView()) setBusy(false); }
  };
  useLayoutEffect(() => {
    active.current = true;
    void refresh();
    return () => { active.current = false; };
    // This editor remounts on credential or authority changes.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  const observe = async (current: Attempt) => {
    setObserved(null);
    try {
      const result = await readRole(current.input.role_name);
      if (!ownsView()) return;
      setObserved(result);
      setNotice(result && roleMatches(result, current.input)
        ? current.accepted ? "roleManagement.confirmed" : "roleManagement.matchesUnknown"
        : result ? "roleManagement.differs" : "roleManagement.absentUnknown");
    } catch { if (ownsView()) setNotice("roleManagement.readbackFailed"); }
  };
  const confirm = async () => {
    if (!ownsView() || working.current || submitted.current || !canCreate || !review || !loaded) return;
    working.current = true; setBusy(true); setNotice(null);
    const input = { ...review, permissions: [...review.permissions] };
    try {
      // Recheck catalog and name before dispatch; this is not a server-side lock.
      const [existing, catalog] = await Promise.all([readRole(input.role_name), listPermissions()]);
      if (!ownsView()) return;
      if (existing || input.permissions.some(code => !catalog.permissions.some(item => item.permission_code === code))) {
        setReview(null); setLoaded(false); setNotice(existing ? "roleManagement.exists" : "roleManagement.catalogChanged"); return;
      }
      const current = { input, accepted: false };
      submitted.current = current; setAttempt(current); setReview(null);
      try { await createRole(input, ownsView); current.accepted = true; } catch { /* A failed response may follow committed writes. */ }
      if (!ownsView()) return;
      setAttempt({ ...current });
      await observe(current);
    } catch { if (ownsView()) { setReview(null); setNotice("roleManagement.preflightFailed"); } }
    finally { working.current = false; if (ownsView()) setBusy(false); }
  };
  const reconcile = async () => {
    if (!ownsView() || working.current || !submitted.current) return;
    working.current = true; setBusy(true);
    try { await observe(submitted.current); }
    finally { working.current = false; if (ownsView()) setBusy(false); }
  };
  return <div className="space-y-5">
    <Link to="/users" className="text-sm text-primary hover:underline">{t("roleManagement.back")}</Link>
    <header className="space-y-2"><h1 className="text-2xl font-semibold">{t("roleManagement.title")}</h1><p className="text-sm text-muted-foreground">{t("roleManagement.scope")}</p></header>
    {notice && <p role="status" className="rounded border p-3 text-sm">{t(notice)}</p>}
    {!canCreate && <p className="text-sm">{t("roleManagement.readOnly")}</p>}
    {attempt ? <section className="space-y-3 rounded-xl border p-4">
      <h2 className="font-semibold">{t("roleManagement.submitted")}</h2>
      <p className="break-all font-mono text-sm">{attempt.input.role_name}</p>
      <p className="text-sm">{t("roleManagement.uncertainty")}</p>
      <Button variant="outline" disabled={busy} onClick={() => void reconcile()}>{t("roleManagement.reconcile")}</Button>
      {observed && <div className="space-y-2"><h3>{t("roleManagement.observed")}</h3><p>{observed.description}</p><ul>{observed.permissions.map(code => <li key={code} className="break-all font-mono text-xs">{code}</li>)}</ul>
        <p className="text-sm">{t("roleManagement.assignmentNotice")}</p><Link to="/users" className="text-sm text-primary hover:underline">{t("roleManagement.assign")}</Link>
      </div>}
    </section> : <>
      <Button variant="outline" disabled={busy} onClick={() => void refresh()}>{t("roleManagement.refresh")}</Button>
      {canCreate && <section className="space-y-3 rounded-xl border p-4"><h2 className="font-semibold">{t("roleManagement.createTitle")}</h2>
        <form className="space-y-3" onSubmit={event => {
          event.preventDefault();
          if (!ownsView() || working.current || submitted.current || !loaded || !canCreate) return;
          if (!/^[a-z_]{1,100}$/.test(name) || !description.trim() || description.trim().length > 255) { setNotice("roleManagement.invalid"); return; }
          if (name === "permissions") { setNotice("roleManagement.reserved"); return; }
          if (selected.some(code => !permissions.some(item => item.permission_code === code))) { setNotice("roleManagement.catalogChanged"); return; }
          setNotice(null); setReview({ role_name: name, description: description.trim(), permissions: [...selected].sort() });
        }}>
          <fieldset disabled={busy || Boolean(review) || !loaded} className="space-y-3">
            <label className="grid gap-1 text-sm">{t("roleManagement.name")}<Input value={name} maxLength={100} required onChange={event => setName(event.target.value)} /></label>
            <p className="text-xs text-muted-foreground">{t("roleManagement.nameHint")}</p>
            <label className="grid gap-1 text-sm">{t("roleManagement.description")}<Input value={description} maxLength={255} required onChange={event => setDescription(event.target.value)} /></label>
            <div className="max-h-72 space-y-2 overflow-y-auto rounded border p-3">{permissions.map(permission => <label key={permission.permission_code} className="flex items-start gap-2 text-sm">
              <input type="checkbox" checked={selected.includes(permission.permission_code)} onChange={event => setSelected(previous => event.target.checked ? [...new Set([...previous, permission.permission_code])] : previous.filter(code => code !== permission.permission_code))} />
              <span><span className="font-mono text-xs">{permission.permission_code}</span><span className="block text-muted-foreground">{permission.description || permission.name}</span></span>
            </label>)}</div>
            <Button type="submit" disabled={!loaded || busy}>{t("roleManagement.review")}</Button>
          </fieldset>
        </form>
        {review && <section aria-label={t("roleManagement.reviewTitle")} className="space-y-3 rounded border border-primary p-3">
          <h3 className="font-semibold">{t("roleManagement.reviewTitle")}</h3><p className="text-sm">{t("roleManagement.scope")}</p>
          <p className="break-all font-mono">{review.role_name}</p><p>{review.description}</p>
          {review.permissions.length ? <ul>{review.permissions.map(code => <li key={code} className="font-mono text-xs">{code}</li>)}</ul> : <p>{t("roleManagement.emptyPermissions")}</p>}
          <p className="text-sm">{t("roleManagement.reviewNotice")}</p>
          <div className="flex gap-3"><Button variant="outline" disabled={busy} onClick={() => setReview(null)}>{t("roleManagement.edit")}</Button><Button disabled={busy} onClick={() => void confirm()}>{t("roleManagement.confirm")}</Button></div>
        </section>}
      </section>}
    </>}
    {loaded && <section className="space-y-3"><h2 className="font-semibold">{t("roleManagement.catalog")}</h2>{roles.map(role => <article key={role.role_name} className="space-y-1 rounded border p-3"><h3 className="font-mono text-sm">{role.role_name}</h3><p className="text-sm">{role.description}</p><p className="text-xs text-muted-foreground">{t(role.is_system ? "roleManagement.system" : "roleManagement.custom")}</p><ul>{role.permissions.map(code => <li key={code} className="break-all font-mono text-xs">{code}</li>)}</ul></article>)}</section>}
  </div>;
}
export function RoleManagementPage() {
  const token = useAuthStore(state => state.token);
  const authority = useAuthStore(state => sdkAuthorityScope(state.user));
  const canRead = useAuthStore(state => state.hasPermission("role:list"));
  const canCreate = useAuthStore(state => Boolean(state.user?.roles.includes("platform_admin")) && state.hasPermission("role:create"));
  const scope = useMemo(() => ({ id: generateUUID(), token, authority }), [token, authority]);
  const isCurrent = () => {
    const state = useAuthStore.getState();
    return state.token === scope.token && sdkAuthorityScope(state.user) === scope.authority;
  };
  const { t } = useTranslation();
  if (!canRead) return <p role="alert">{t("roleManagement.denied")}</p>;
  return <RoleEditor key={scope.id} isCurrent={isCurrent} canCreate={canCreate} />;
}
