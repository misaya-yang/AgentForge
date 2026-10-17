import { useState } from "react";
import { useLocation, useNavigate } from "react-router-dom";
import { useTranslation } from "react-i18next";
import { motion, useReducedMotion } from "motion/react";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { getAllowedEmailDomain, getSupportEmail } from "@/config/runtime";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Logo } from "@/components/Logo";
import { useAuthStore } from "@/store/useAuthStore";
import { login } from "@/api/auth";
import { PasswordChangeModal } from "@/components/PasswordChangeModal";
import {
  Activity,
  ArrowRight,
  BookOpen,
  CheckCircle2,
  Cpu,
  Eye,
  EyeOff,
  Lock,
  Mail,
  Network,
  Radio,
  Server,
  Shield,
  ShieldCheck,
  Sparkles,
  Zap,
} from "lucide-react";
import { Modal } from "antd";
import type { AxiosError } from "axios";

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
  const shouldReduceMotion = useReducedMotion();
  const allowedDomain = getAllowedEmailDomain();
  const supportEmail = getSupportEmail();
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [error, setError] = useState("");
  const [showPasswordChange, setShowPasswordChange] = useState(false);
  const [showPassword, setShowPassword] = useState(false);
  const [rememberMe, setRememberMe] = useState(false);
  const returnPath = getSafeReturnPath(location.state);
  const hasFullEmail = email.trim().includes("@");

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault(); setError(""); setLoading(true);
    try {
      const trimmed = email.trim();
      if (!trimmed) { setError(t("login.errors.emailRequired")); setLoading(false); return; }
      const normalized = trimmed.includes("@") ? trimmed.toLowerCase() : `${trimmed.toLowerCase()}@${allowedDomain}`;
      const res = await login({ email: normalized, password });
      setAuth(res.access_token, res.user, res.force_password_change, rememberMe);
      if (res.force_password_change) {
        setShowPasswordChange(true);
      } else {
        navigate(returnPath || getDefaultDestination(res.user.permissions || []), { replace: true });
      }
    } catch (err: unknown) {
      const ax = err as AxiosError<ApiErrorPayload>;
      const status = ax.response?.status;
      const data = ax.response?.data;
      const msg = extractErrorText(data?.detail ?? data);
      if (status === 423) setError(msg || t("login.errors.accountLocked"));
      else if (status === 401) setError(t("login.errors.invalidCredentials"));
      else if (status === 403) setError(t("login.errors.accountDisabled"));
      else setError(msg || t("login.errors.loginFailed"));
    } finally { setLoading(false); }
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

  const highlights = [
    {
      icon: Server,
      title: t("login.highlights.operations.title"),
      description: t("login.highlights.operations.description"),
      tag: "LLM Control Plane",
    },
    {
      icon: BookOpen,
      title: t("login.highlights.knowledge.title"),
      description: t("login.highlights.knowledge.description"),
      tag: "Hybrid RAG Pipeline",
    },
    {
      icon: Activity,
      title: t("login.highlights.observability.title"),
      description: t("login.highlights.observability.description"),
      tag: "Full-link Tracing",
    },
  ];

  const entranceTransition = {
    duration: shouldReduceMotion ? 0 : 0.45,
    ease: [0.16, 1, 0.3, 1] as const,
  };

  return (
    <div className="relative min-h-dvh w-full overflow-x-hidden bg-[#07080b] text-foreground selection:bg-primary/30 selection:text-white">
      {/* Ambient background lighting & engineering mesh grid */}
      <div className="pointer-events-none absolute inset-0 z-0 overflow-hidden">
        {/* Subtle radial gradients */}
        <div className="absolute -left-[10%] -top-[15%] h-[680px] w-[680px] rounded-full bg-indigo-600/12 blur-[130px]" />
        <div className="absolute right-[5%] top-[10%] h-[550px] w-[550px] rounded-full bg-violet-600/10 blur-[140px]" />
        <div className="absolute bottom-[-10%] left-[30%] h-[600px] w-[600px] rounded-full bg-cyan-600/8 blur-[150px]" />

        {/* Precision grid pattern */}
        <div className="absolute inset-0 bg-[linear-gradient(to_right,#ffffff05_1px,transparent_1px),linear-gradient(to_bottom,#ffffff05_1px,transparent_1px)] bg-[size:40px_40px] [mask-image:radial-gradient(ellipse_65%_65%_at_50%_45%,#000_70%,transparent_100%)]" />
      </div>

      <main className="relative z-10 mx-auto grid min-h-dvh w-full max-w-[1440px] items-center px-4 py-8 sm:px-8 lg:grid-cols-[1.18fr_0.82fr] lg:gap-12 lg:px-12 xl:gap-20 xl:px-16">
        {/* Left Side: Enterprise Control Plane Showcase (Desktop) */}
        <motion.div
          initial={shouldReduceMotion ? false : { opacity: 0, y: 16 }}
          animate={{ opacity: 1, y: 0 }}
          transition={entranceTransition}
          className="hidden min-h-[580px] flex-col justify-between py-6 lg:flex lg:py-10"
        >
          {/* Header Bar */}
          <div className="flex items-center gap-3">
            <Logo collapsed={false} textClassName="text-[15px] font-semibold text-white leading-none" />
            <span className="inline-flex items-center gap-1.5 rounded-full border border-indigo-500/20 bg-indigo-500/10 px-2.5 py-0.5 text-[11px] font-medium tracking-wide text-indigo-300">
              <span className="h-1.5 w-1.5 rounded-full bg-emerald-400 animate-pulse" />
              v2.4 Enterprise
            </span>
          </div>

          {/* Headline & Value Proposition */}
          <div className="my-8 lg:my-10">
            <div className="inline-flex items-center gap-2 rounded-md border border-white/10 bg-white/[0.03] px-3 py-1 text-xs font-mono tracking-wider text-indigo-400">
              <Sparkles size={13} className="text-indigo-400" />
              {t("login.brandEyebrow")}
            </div>

            <h1 className="mt-4 text-3xl font-bold tracking-tight text-white sm:text-4xl lg:text-[40px] lg:leading-[1.16]">
              {t("login.brandTitle")}
            </h1>

            <p className="mt-4 max-w-xl text-sm leading-relaxed text-zinc-400 sm:text-base">
              {t("login.brandDescription")}
            </p>

            {/* Gateway Architecture Preview */}
            <div className="mt-8 overflow-hidden rounded-2xl border border-white/10 bg-zinc-950/60 p-5 shadow-2xl backdrop-blur-xl">
              <div className="flex flex-wrap items-center justify-between gap-3 border-b border-white/5 pb-3">
                <div className="flex items-center gap-2">
                  <div className="flex gap-1.5">
                    <span className="h-2.5 w-2.5 rounded-full bg-red-500/60" />
                    <span className="h-2.5 w-2.5 rounded-full bg-amber-500/60" />
                    <span className="h-2.5 w-2.5 rounded-full bg-emerald-500/60" />
                  </div>
                  <span className="ml-2 font-mono text-xs text-zinc-400">
                    AI Gateway / architecture overview
                  </span>
                </div>
                <div className="flex items-center gap-3 font-mono text-xs">
                  <span className="inline-flex items-center gap-1 text-emerald-400">
                    <Radio size={12} className="animate-pulse" />
                    DEPLOYMENT OVERVIEW
                  </span>
                  <span className="text-zinc-500">|</span>
                  <span className="text-zinc-400">Metrics after sign-in</span>
                </div>
              </div>

              {/* Architecture Pipeline Flow */}
              <div className="my-4 grid grid-cols-3 gap-2 text-center text-xs">
                <div className="rounded-lg border border-white/5 bg-white/[0.02] p-2.5">
                  <div className="flex items-center justify-center gap-1.5 font-medium text-zinc-300">
                    <Zap size={13} className="text-amber-400" />
                    Inbound Traffic
                  </div>
                  <div className="mt-1 font-mono text-[11px] text-zinc-500">
                    Request routing
                  </div>
                </div>

                <div className="rounded-lg border border-indigo-500/30 bg-indigo-500/[0.06] p-2.5">
                  <div className="flex items-center justify-center gap-1.5 font-medium text-indigo-300">
                    <Shield size={13} className="text-indigo-400" />
                    Auth & Security
                  </div>
                  <div className="mt-1 font-mono text-[11px] text-emerald-400">
                    Scoped authorization
                  </div>
                </div>

                <div className="rounded-lg border border-white/5 bg-white/[0.02] p-2.5">
                  <div className="flex items-center justify-center gap-1.5 font-medium text-zinc-300">
                    <Cpu size={13} className="text-cyan-400" />
                    Model Fleet
                  </div>
                  <div className="mt-1 font-mono text-[11px] text-zinc-500">
                    Configured providers
                  </div>
                </div>
              </div>

              {/* Micro telemetry footer */}
              <div className="flex items-center justify-between rounded-lg bg-black/40 px-3 py-2 font-mono text-[11px] text-zinc-400">
                <span className="flex items-center gap-1.5">
                  <Network size={12} className="text-indigo-400" />
                  Route: <code className="text-zinc-200">/v1/chat/completions</code> ➔ <span className="text-indigo-300">Gateway routing</span>
                </span>
                <span className="text-emerald-400 flex items-center gap-1">
                  <CheckCircle2 size={11} /> Architecture preview
                </span>
              </div>
            </div>

            {/* Feature Highlights Grid */}
            <div className="mt-6 grid grid-cols-1 gap-3 sm:grid-cols-3">
              {highlights.map(({ icon: Icon, title, description }) => (
                <div
                  key={title}
                  className="group rounded-xl border border-white/5 bg-white/[0.02] p-3.5 transition-[border-color,background-color] duration-200 hover:border-indigo-500/30 hover:bg-white/[0.04]"
                >
                  <div className="flex items-center gap-2.5">
                    <div className="flex h-7 w-7 shrink-0 items-center justify-center rounded-md border border-white/10 bg-white/[0.04] text-indigo-400 group-hover:border-indigo-500/40 group-hover:text-indigo-300">
                      <Icon size={14} strokeWidth={1.7} />
                    </div>
                    <span className="text-xs font-semibold text-zinc-200">{title}</span>
                  </div>
                  <p className="mt-2 text-[12px] leading-relaxed text-zinc-400 line-clamp-2">
                    {description}
                  </p>
                </div>
              ))}
            </div>
          </div>

          {/* Trust Footer */}
          <div className="flex flex-wrap items-center gap-4 text-xs text-zinc-500">
            <span className="flex items-center gap-1.5">
              <ShieldCheck size={14} className="text-emerald-400" />
              {t("login.secureAccess")}
            </span>
            <span>•</span>
            <span>Self-hosted deployment</span>
            <span>•</span>
            <span>Zero-Trust Gateway Architecture</span>
          </div>
        </motion.div>

        {/* Right Side: Glassmorphic Login Workbench Card */}
        <motion.div
          initial={shouldReduceMotion ? false : { opacity: 0, y: 16 }}
          animate={{ opacity: 1, y: 0 }}
          transition={{ ...entranceTransition, delay: shouldReduceMotion ? 0 : 0.08 }}
          className="relative mx-auto w-full max-w-[460px]"
        >
          {/* Mobile Logo Header */}
          <div className="mb-6 flex items-center justify-center gap-3 lg:hidden">
            <Logo collapsed={false} textClassName="text-[15px] font-semibold text-white leading-none" />
            <span className="inline-flex items-center gap-1.5 rounded-full border border-indigo-500/20 bg-indigo-500/10 px-2.5 py-0.5 text-[11px] font-medium tracking-wide text-indigo-300">
              <span className="h-1.5 w-1.5 rounded-full bg-emerald-400 animate-pulse" />
              v2.4 Enterprise
            </span>
          </div>

          {/* Card subtle glowing border gradient */}
          <div className="relative overflow-hidden rounded-2xl border border-white/15 bg-zinc-900/80 p-7 shadow-[0_20px_70px_rgba(0,0,0,0.6)] backdrop-blur-2xl sm:p-9">
            {/* Top jewel gradient bar */}
            <div className="absolute left-0 right-0 top-0 h-[2px] bg-gradient-to-r from-transparent via-indigo-500 to-transparent opacity-80" />

            <div className="mb-7">
              <div className="inline-flex items-center gap-1.5 rounded-md border border-white/10 bg-white/[0.04] px-2.5 py-0.5 text-[11px] font-mono text-zinc-400">
                <Lock size={11} className="text-indigo-400" />
                ENTERPRISE WORKSPACE
              </div>
              <h2 className="mt-3 text-2xl font-bold tracking-tight text-white sm:text-3xl">
                {t("login.title")}
              </h2>
              <p className="mt-1.5 text-xs text-zinc-400 sm:text-sm">
                {t("login.loginWith")}
              </p>
            </div>

            <form onSubmit={handleSubmit} className="space-y-4">
              {/* Email Input */}
              <div className="space-y-1.5">
                <Label htmlFor="email" className="text-xs font-medium text-zinc-300">
                  {t("login.emailLabel")}
                </Label>
                <div className="relative flex h-11 items-stretch overflow-hidden rounded-lg border border-white/10 bg-black/40 transition-[border-color,box-shadow] duration-150 focus-within:border-indigo-500 focus-within:ring-2 focus-within:ring-indigo-500/20">
                  <div className="flex w-10 shrink-0 items-center justify-center text-zinc-500">
                    <Mail size={16} />
                  </div>
                  <Input
                    id="email"
                    type="text"
                    inputMode="email"
                    autoComplete="username"
                    autoCapitalize="none"
                    spellCheck={false}
                    placeholder={t("login.emailPlaceholder")}
                    value={email}
                    onChange={(event) => setEmail(event.target.value)}
                    required
                    disabled={isLoading}
                    aria-invalid={Boolean(error)}
                    aria-describedby={error ? "login-email-hint login-error" : "login-email-hint"}
                    className="h-full w-0 min-w-0 flex-1 rounded-none border-0 bg-transparent px-1 text-sm text-white shadow-none placeholder:text-zinc-600 focus-visible:border-0 focus-visible:ring-0"
                  />
                  {!hasFullEmail && (
                    <span
                      className="flex max-w-[55%] shrink-0 items-center border-l border-white/10 bg-white/[0.03] px-3 font-mono text-[12px] text-zinc-400"
                      title={`@${allowedDomain}`}
                      aria-hidden="true"
                    >
                      <span className="truncate">@{allowedDomain}</span>
                    </span>
                  )}
                </div>
                <p id="login-email-hint" className="sr-only">
                  {t("login.emailHint", { domain: allowedDomain })}
                </p>
              </div>

              {/* Password Input */}
              <div className="space-y-1.5">
                <Label htmlFor="password" className="text-xs font-medium text-zinc-300">
                  {t("login.passwordLabel")}
                </Label>
                <div className="relative flex h-11 items-stretch overflow-hidden rounded-lg border border-white/10 bg-black/40 transition-[border-color,box-shadow] duration-150 focus-within:border-indigo-500 focus-within:ring-2 focus-within:ring-indigo-500/20">
                  <div className="flex w-10 shrink-0 items-center justify-center text-zinc-500">
                    <Lock size={16} />
                  </div>
                  <Input
                    id="password"
                    type={showPassword ? "text" : "password"}
                    autoComplete="current-password"
                    placeholder={t("login.passwordPlaceholder")}
                    value={password}
                    onChange={(event) => setPassword(event.target.value)}
                    required
                    disabled={isLoading}
                    aria-invalid={Boolean(error)}
                    aria-describedby={error ? "login-error" : undefined}
                    className="h-full w-0 min-w-0 flex-1 rounded-none border-0 bg-transparent px-1 pr-10 text-sm text-white shadow-none placeholder:text-zinc-600 focus-visible:border-0 focus-visible:ring-0"
                  />
                  <button
                    type="button"
                    aria-label={showPassword ? t("login.hidePassword") : t("login.showPassword")}
                    onClick={() => setShowPassword((current) => !current)}
                    className="absolute right-2.5 top-1/2 -translate-y-1/2 rounded p-1.5 text-zinc-500 transition-colors hover:bg-white/5 hover:text-zinc-300 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-indigo-500/30"
                  >
                    {showPassword ? <EyeOff size={16} /> : <Eye size={16} />}
                  </button>
                </div>
              </div>

              {/* Remember Me & Forgot Password */}
              <div className="flex flex-wrap items-center justify-between gap-3 pt-1 text-xs">
                <label
                  htmlFor="remember-me"
                  className="flex cursor-pointer items-center gap-2 text-zinc-400 hover:text-zinc-200 transition-colors"
                >
                  <Checkbox
                    id="remember-me"
                    checked={rememberMe}
                    onCheckedChange={(checked) => setRememberMe(checked === true)}
                    className="border-white/20 data-[state=checked]:bg-indigo-600 data-[state=checked]:border-indigo-600"
                  />
                  {t("login.rememberMeDays")}
                </label>
                <button
                  type="button"
                  onClick={handleForgotPassword}
                  className="rounded text-indigo-400 transition-colors hover:text-indigo-300 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-indigo-500/30"
                >
                  {t("login.forgotPassword")}
                </button>
              </div>

              {/* Error Message */}
              {error && (
                <div
                  id="login-error"
                  role="alert"
                  aria-live="polite"
                  className="rounded-lg border border-red-500/30 bg-red-500/10 px-3.5 py-2.5 text-xs text-red-300"
                >
                  {error}
                </div>
              )}

              {/* Submit Button */}
              <Button
                type="submit"
                className="group relative h-11 w-full overflow-hidden rounded-lg bg-gradient-to-r from-indigo-500 via-indigo-600 to-violet-600 text-sm font-semibold text-white shadow-[0_4px_20px_rgba(99,102,241,0.35)] transition-[transform,opacity,box-shadow] duration-150 hover:from-indigo-400 hover:to-violet-500 hover:shadow-[0_6px_24px_rgba(99,102,241,0.5)] active:scale-[0.99] disabled:opacity-50"
                disabled={isLoading}
              >
                <span className="flex items-center justify-center gap-2">
                  {isLoading ? t("login.loggingIn") : t("login.loginButton")}
                  {!isLoading && (
                    <ArrowRight size={15} className="transition-transform group-hover:translate-x-1" />
                  )}
                </span>
              </Button>

              <div className="pt-2 text-center">
                <p className="text-[11px] text-zinc-500">
                  {t("login.enterpriseOnly", { domain: allowedDomain })}
                </p>
              </div>
            </form>
          </div>
        </motion.div>
      </main>

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
