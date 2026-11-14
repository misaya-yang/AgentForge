import { consoleLoginFence } from "@/lib/consoleLoginFence";
import { useEffect, useRef, useState } from "react";
import { EnterpriseSignIn } from "@/components/EnterpriseSignIn";
import { useLocation, useNavigate } from "react-router-dom";
import { useTranslation } from "react-i18next";
import { getAllowedEmailDomain, getSupportEmail } from "@/config/runtime";
import { Logo } from "@/components/Logo";
import { consoleAuthGeneration, useAuthStore } from "@/store/useAuthStore";
import { login } from "@/api/auth";
import { PasswordChangeModal } from "@/components/PasswordChangeModal";
import { ArrowRight, Eye, EyeOff, LoaderCircle, ShieldCheck } from "lucide-react";
import { Modal } from "@/components/ui/native-modal";
import type { HttpError } from '@/lib/fetchClient';

type ApiValidationIssue = { msg?: unknown; message?: unknown };
type ApiErrorPayload = { detail?: unknown; message?: unknown; error?: unknown };

function sanitizeMessage(value: string): string {
  return value.replace(/^Value error,\s*/i, "").trim();
}

function extractErrorText(payload: unknown): string | null {
  if (!payload) return null;
  if (typeof payload === "string") { const t = sanitizeMessage(payload); return t || null; }
  if (Array.isArray(payload)) {
    const msgs = payload.map(item => {
      if (typeof item === "string") return sanitizeMessage(item);
      if (!item || typeof item !== "object") return "";
      const i = item as ApiValidationIssue;
      if (typeof i.msg === "string") return sanitizeMessage(i.msg);
      if (typeof i.message === "string") return sanitizeMessage(i.message);
      return "";
    }).filter(Boolean);
    return msgs.length > 0 ? msgs.join("; ") : null;
  }
  if (typeof payload === "object") {
    const o = payload as ApiValidationIssue;
    if (typeof o.msg === "string") return sanitizeMessage(o.msg);
    if (typeof o.message === "string") return sanitizeMessage(o.message);
  }
  return null;
}

function getSafeReturnPath(state: unknown): string | null {
  if (!state || typeof state !== "object") return null;
  const from = (state as { from?: unknown }).from;
  if (!from || typeof from !== "object") return null;

  const { pathname, search, hash } = from as {
    pathname?: unknown;
    search?: unknown;
    hash?: unknown;
  };
  if (
    typeof pathname !== "string"
    || !pathname.startsWith("/")
    || pathname.startsWith("//")
    || pathname === "/login"
  ) {
    return null;
  }

  const safeSearch = typeof search === "string" && (search === "" || search.startsWith("?"))
    ? search
    : "";
  const safeHash = typeof hash === "string" && (hash === "" || hash.startsWith("#"))
    ? hash
    : "";
  return `${pathname}${safeSearch}${safeHash}`;
}

function getDefaultDestination(permissions: string[]): string {
  const hasDashboard = permissions.includes("console:dashboard:view") || permissions.includes("admin:*");
  const hasPlayground = permissions.includes("conversation:playground:access") || permissions.includes("admin:*");
  return !hasDashboard && hasPlayground ? "/playground" : "/";
}

export function LoginPage() {
  const navigate = useNavigate();
  const location = useLocation();
  const { t } = useTranslation();
  const { setAuth, setLoading, isLoading } = useAuthStore();
  const allowedDomain = getAllowedEmailDomain();
  const supportEmail = getSupportEmail();
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [error, setError] = useState("");
  const [showPasswordChange, setShowPasswordChange] = useState(false);
  const [showPassword, setShowPassword] = useState(false);
  const returnPath = getSafeReturnPath(location.state);
  const hasFullEmail = email.trim().includes("@");
  const pendingLogin = useRef(false), mountedLogin = useRef(true);
  useEffect(() => { mountedLogin.current = true; return () => { mountedLogin.current = false; }; }, []);

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    const initial = useAuthStore.getState();
    if (pendingLogin.current || initial.isLoading) return;
    pendingLogin.current = true;
    const fence = consoleLoginFence(useAuthStore.getState, consoleAuthGeneration, useAuthStore.subscribe, () => mountedLogin.current);
    const current = fence.current;
    setError(""); setLoading(true);
    try {
      const trimmed = email.trim();
      if (!trimmed) { setError(t("login.errors.emailRequired")); setLoading(false); return; }
      const normalized = trimmed.includes("@") ? trimmed.toLowerCase() : `${trimmed.toLowerCase()}@${allowedDomain}`;
      const res = await login({ email: normalized, password });
      if (!current()) return;
      setAuth(res.csrf_token, res.user, res.force_password_change);
      if (res.force_password_change) {
        setShowPasswordChange(true);
      } else {
        navigate(returnPath || getDefaultDestination(res.user.permissions || []), { replace: true });
      }
    } catch (err: unknown) {
      const ax = err as HttpError<ApiErrorPayload>;
      if (!fence.ownsFailure(ax.response?.status)) return;
      const status = ax.response?.status;
      const data = ax.response?.data;
      const msg = extractErrorText(data?.detail ?? data);
      if (status === 423) setError(msg || t("login.errors.accountLocked"));
      else if (status === 401) setError(t("login.errors.invalidCredentials"));
      else if (status === 403) setError(t("login.errors.accountDisabled"));
      else setError(msg || t("login.errors.loginFailed"));
    } finally { pendingLogin.current = false; if (current()) setLoading(false); fence.dispose(); }
  };

  const handleForgotPassword = () => {
    Modal.info({
      title: t("login.forgotPasswordModal.title"),
      content: (
        <div className="py-2">
          <p>{t("login.forgotPasswordModal.content")}</p>
          <p className="mt-2 text-sm text-muted-foreground">{t("login.forgotPasswordModal.adminEmail", { email: supportEmail })}</p>
        </div>
      ),
      okText: t("login.forgotPasswordModal.ok"),
    });
  };

  return (
    <div className="forge-login">
      <header className="forge-login-brand"><Logo collapsed={false} /></header>
      <main className="forge-login-main">
        <section className="forge-login-panel" aria-labelledby="login-title">
          <h1 id="login-title">{t("login.title")}</h1>
          <p className="forge-login-summary">{t("login.loginWith")}</p>
          <form onSubmit={handleSubmit} className="forge-login-form">
            <div className="forge-login-field">
              <label htmlFor="email">{t("login.emailLabel")}</label>
              <div className="forge-login-email">
                <input className="forge-input" id="email" name="username" type="text" inputMode="email" autoComplete="username" autoCapitalize="none" spellCheck={false} required disabled={isLoading}
                  value={email} onChange={event => setEmail(event.target.value)} placeholder={t("login.emailPlaceholder")}
                  aria-invalid={Boolean(error)} aria-describedby={error ? "login-email-hint login-error" : "login-email-hint"} />
                {!hasFullEmail && <span aria-hidden="true">@{allowedDomain}</span>}
              </div>
              <p id="login-email-hint">{t("login.emailHint", { domain: allowedDomain })}</p>
            </div>
            <div className="forge-login-field">
              <label htmlFor="password">{t("login.passwordLabel")}</label>
              <div className="forge-login-password">
                <input className="forge-input" id="password" name="password" type={showPassword ? "text" : "password"} autoComplete="current-password" required disabled={isLoading}
                  value={password} onChange={event => setPassword(event.target.value)} placeholder={t("login.passwordPlaceholder")}
                  aria-invalid={Boolean(error)} aria-describedby={error ? "login-error" : undefined} />
                <button type="button" className="forge-icon-button" aria-label={showPassword ? t("login.hidePassword") : t("login.showPassword")} aria-pressed={showPassword} onClick={() => setShowPassword(current => !current)}>
                  {showPassword ? <EyeOff size={18} aria-hidden="true" /> : <Eye size={18} aria-hidden="true" />}
                </button>
              </div>
            </div>
            <button type="button" className="forge-login-recovery" onClick={handleForgotPassword}>{t("login.forgotPassword")}</button>
            {error && <p id="login-error" role="alert" className="forge-login-error">{error}</p>}
            <button type="submit" className="forge-button forge-button-primary forge-login-submit" disabled={isLoading} aria-busy={isLoading}>
              {isLoading ? <LoaderCircle className="connect-spinner" size={18} aria-hidden="true" /> : null}
              {isLoading ? t("login.loggingIn") : t("login.loginButton")}
              {!isLoading ? <ArrowRight size={18} aria-hidden="true" /> : null}
            </button>
            <p className="forge-login-policy">{t("login.cookieSessionLifetime")}</p>
          </form>
          <EnterpriseSignIn onAuthenticated={grant => {
            setAuth(grant.csrf_token, grant.user, grant.force_password_change);
            if (grant.force_password_change) setShowPasswordChange(true);
            else navigate(returnPath || getDefaultDestination(grant.user.permissions), { replace: true });
          }} />
        </section>
      </main>
      <footer className="forge-login-footer"><ShieldCheck size={16} aria-hidden="true" />{t("login.secureAccess")}</footer>
      <PasswordChangeModal open={showPasswordChange} onComplete={() => {
        setShowPasswordChange(false);
        const user = useAuthStore.getState().user;
        if (!user) {
          navigate("/login", { replace: true });
          return;
        }
        navigate(returnPath || getDefaultDestination(user?.permissions || []), { replace: true });
      }} />
    </div>
  );
}
