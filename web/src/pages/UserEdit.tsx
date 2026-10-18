import { requestDepartment } from "@/api/users";
import { useState, useEffect, useMemo } from "react";
import { useParams, useNavigate, Link } from "react-router-dom";
import { useTranslation } from "react-i18next";
import {
  ArrowLeftOutlined,
  UserOutlined,
  MailOutlined,
  TeamOutlined,
  SafetyCertificateOutlined,
  KeyOutlined,
  CheckOutlined,
  LoadingOutlined,
  ExclamationCircleOutlined,
  DownOutlined,
} from "@ant-design/icons";
import { message, Spin, Tooltip } from "antd";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Badge } from "@/components/ui/badge";
import { Checkbox } from "@/components/ui/checkbox";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { useAuthStore } from "@/store/useAuthStore";
import { canAdministerAccounts, isProtectedAccount } from "@/lib/accountAdministration";
import { useAccountMutation } from "@/hooks/useAccountMutation";
import {
  getUser,
  updateUser,
  listRoles,
  listPermissions,
} from "@/api/users";
import { listServices } from "@/api/gateway";
import type { UserResponse, RoleResponse, PermissionResponse } from "@/api/users";
import type { ServiceDefinition } from "@/types/gateway";

const buildDepartmentOptions = (t: (key: string, options?: Record<string, unknown>) => string) => [
  { value: "cs", label: t("user.departments.cs") },
  { value: "sales", label: t("user.departments.sales") },
  { value: "tech", label: t("user.departments.tech") },
  { value: "admin", label: t("user.departments.admin") },
];

const buildStatusOptions = (t: (key: string, options?: Record<string, unknown>) => string) => [
  { value: "active", label: t("users.status.active") },
  { value: "disabled", label: t("users.status.disabled") },
];

type PermissionCategoryMeta = {
  label: string;
};

const buildCategoryMeta = (
  t: (key: string, options?: Record<string, unknown>) => string
): Record<string, PermissionCategoryMeta> => ({
  console: { label: t("users.permissions.categories.console") },
  conversation: { label: t("users.permissions.categories.conversation") },
  service: { label: t("users.permissions.categories.service") },
  knowledge: { label: t("users.permissions.categories.knowledge") },
  user: { label: t("users.permissions.categories.user") },
  role: { label: t("users.permissions.categories.role") },
  admin: { label: t("users.permissions.categories.admin") },
});

export function UserEditPage() {
  const { userId } = useParams<{ userId: string }>();
  const actor = useAuthStore(state => state.user);
  return <UserEditContent key={JSON.stringify([userId, actor?.user_id, actor?.roles])} />;
}

function UserEditContent() {
  const { t } = useTranslation();
  const { userId } = useParams<{ userId: string }>();
  const navigate = useNavigate();
  const { hasPermission, user: currentUser } = useAuthStore();
  const departmentOptions = useMemo(() => buildDepartmentOptions(t), [t]);
  const statusOptions = useMemo(() => buildStatusOptions(t), [t]);
  const categoryMeta = useMemo(() => buildCategoryMeta(t), [t]);

  // Data states
  const [user, setUser] = useState<UserResponse | null>(null);
  const [roles, setRoles] = useState<RoleResponse[]>([]);
  const [permissions, setPermissions] = useState<PermissionResponse[]>([]);
  const [services, setServices] = useState<ServiceDefinition[]>([]);
  const [isLoading, setIsLoading] = useState(true);
  const [isSaving, setIsSaving] = useState(false);
  const mutation = useAccountMutation(t("users.edit.saveFailed"));
  const refreshed = mutation.refreshed;
  const [reloadRevision, setReloadRevision] = useState(0);
  const [servicesLoading, setServicesLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Form states
  const [formDisplayName, setFormDisplayName] = useState("");
  const [formDepartment, setFormDepartment] = useState<string>("");
  const [formStatus, setFormStatus] = useState("active");
  const [formRoles, setFormRoles] = useState<string[]>([]);
  const [formExtraPermissions, setFormExtraPermissions] = useState<string[]>([]);
  const [serviceAccessMode, setServiceAccessMode] = useState<"all" | "allowlist">("all");
  const [formAllowedServices, setFormAllowedServices] = useState<string[]>([]);
  const [formDeniedServices, setFormDeniedServices] = useState<string[]>([]);

  // UI states
  const [expandedCategories, setExpandedCategories] = useState<Set<string>>(new Set(["console"]));
  const [hasChanges, setHasChanges] = useState(false);

  // Permissions check
  const canEdit = canAdministerAccounts(currentUser) && hasPermission("user:edit")
    && Boolean(user && !isProtectedAccount(user, currentUser?.user_id));
  const canViewRoles = hasPermission("role:list");
  const canViewServices =
    hasPermission("console:services:view") || hasPermission("service:view") || hasPermission("admin:*");

  // Load data
  useEffect(() => {
    if (!userId) return;

    const loadData = async () => {
      setIsLoading(true);
      setServicesLoading(true);
      setError(null);
      try {
        const [userData, rolesData, permsData, servicesData] = await Promise.all([
          getUser(userId),
          canViewRoles ? listRoles() : Promise.resolve({ roles: [] }),
          canViewRoles ? listPermissions() : Promise.resolve({ permissions: [] }),
          canViewServices ? listServices() : Promise.resolve([] as ServiceDefinition[]),
        ]);

        setUser(userData);
        setRoles(rolesData.roles.filter(role => role.role_name !== "platform_admin"));
        setPermissions(permsData.permissions);
        setServices(
          servicesData.filter((svc) => Boolean(svc.service_id) && svc.service_id !== "assistant")
        );

        // Initialize form
        setFormDisplayName(userData.display_name || "");
        setFormDepartment(userData.department || "");
        setFormStatus(userData.status);
        setFormRoles(userData.roles);
        setFormExtraPermissions(userData.extra_permissions || []);
        setServiceAccessMode(userData.service_access_mode || "all");
        setFormAllowedServices(userData.allowed_services || []);
        setFormDeniedServices(userData.denied_services || []);
        refreshed();
      } catch (err) {
        console.error("Failed to load user:", err);
        setError(t("users.edit.loadFailed"));
      } finally {
        setIsLoading(false);
        setServicesLoading(false);
      }
    };

    loadData();
  }, [userId, canViewRoles, canViewServices, t, reloadRevision, refreshed]);

  // Track changes
  useEffect(() => {
    if (!user) return;
    const changed =
      formDisplayName !== (user.display_name || "") ||
      formDepartment !== (user.department || "") ||
      formStatus !== user.status ||
      JSON.stringify([...formRoles].sort()) !== JSON.stringify([...user.roles].sort()) ||
      JSON.stringify([...formExtraPermissions].sort()) !== JSON.stringify([...(user.extra_permissions || [])].sort()) ||
      serviceAccessMode !== (user.service_access_mode || "all") ||
      JSON.stringify([...formAllowedServices].sort()) !== JSON.stringify([...(user.allowed_services || [])].sort()) ||
      JSON.stringify([...formDeniedServices].sort()) !== JSON.stringify([...(user.denied_services || [])].sort());
    setHasChanges(changed);
  }, [
    user,
    formDisplayName,
    formDepartment,
    formStatus,
    formRoles,
    formExtraPermissions,
    serviceAccessMode,
    formAllowedServices,
    formDeniedServices,
  ]);

  // Group permissions by category
  const permissionsByCategory = useMemo(() => {
    const grouped: Record<string, PermissionResponse[]> = {};
    permissions.forEach((perm) => {
      const cat = perm.category || "other";
      if (!grouped[cat]) grouped[cat] = [];
      grouped[cat].push(perm);
    });
    return grouped;
  }, [permissions]);

  // Get permissions from selected roles
  const rolePermissions = useMemo(() => {
    const perms = new Set<string>();
    const selectedRoles = new Set(formRoles);
    roles.forEach((role) => {
      if (selectedRoles.has(role.role_name)) {
        role.permissions.forEach((p) => perms.add(p));
      }
    });
    return perms;
  }, [formRoles, roles]);

  // Toggle category expansion
  const toggleCategory = (category: string) => {
    setExpandedCategories((prev) => {
      const next = new Set(prev);
      if (next.has(category)) {
        next.delete(category);
      } else {
        next.add(category);
      }
      return next;
    });
  };

  const toggleAllowedService = (serviceId: string) => {
    setFormAllowedServices((prev) => (
      prev.includes(serviceId)
        ? prev.filter((item) => item !== serviceId)
        : [...prev, serviceId]
    ));
    setFormDeniedServices((prev) => prev.filter((item) => item !== serviceId));
  };

  const toggleDeniedService = (serviceId: string) => {
    setFormDeniedServices((prev) => (
      prev.includes(serviceId)
        ? prev.filter((item) => item !== serviceId)
        : [...prev, serviceId]
    ));
    setFormAllowedServices((prev) => prev.filter((item) => item !== serviceId));
  };

  // Handle save
  const handleSave = async () => {
    if (!userId || !canEdit || isSaving || mutation.blocked) return;
    setIsSaving(true);
    try {
      const servicePolicyPatch = canViewServices
        ? {
            service_access_mode: serviceAccessMode,
            allowed_services: formAllowedServices,
            denied_services: formDeniedServices,
          }
        : {};

      if (!await mutation.run(() => updateUser(userId, {
        display_name: formDisplayName,
        department: requestDepartment(formDepartment),
        status: formStatus,
        roles: formRoles,
        extra_permissions: formExtraPermissions,
        ...servicePolicyPatch,
      }))) return;
      message.success(t("users.edit.saved"));
      setHasChanges(false);
      setReloadRevision(value => value + 1);
    } catch (err: unknown) {
      const httpError = err as { response?: { data?: { detail?: string } } };
      message.error(httpError.response?.data?.detail || t("users.edit.saveFailed"));
    } finally {
      setIsSaving(false);
    }
  };

  if (isLoading) {
    return (
      <div
        className="flex min-h-[400px] items-center justify-center"
        role="status"
        aria-live="polite"
        aria-label={t("common.loading")}
      >
        <Spin size="large" />
      </div>
    );
  }

  if (error || !user) {
    return (
      <div
        className="flex min-h-[400px] flex-col items-center justify-center gap-4 px-4 text-center"
        role="alert"
      >
        <ExclamationCircleOutlined className="text-4xl text-destructive" aria-hidden="true" />
        <p className="text-base text-muted-foreground">{error || t("users.edit.notFound")}</p>
        <Button variant="outline" onClick={() => navigate("/users")}>
          {t("users.edit.backToList")}
        </Button>
      </div>
    );
  }

  const dept = departmentOptions.find((d) => d.value === user.department);
  const statusOpt = statusOptions.find((s) => s.value === user.status);

  return (
    <div className="pb-28 sm:pb-24">
      {/* Breadcrumb */}
      <nav className="mb-4 flex items-center gap-2 text-sm" aria-label="Breadcrumb">
        <Link
          to="/users"
          className="flex items-center gap-1 rounded-sm text-muted-foreground transition-colors hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/40 motion-reduce:transition-none"
        >
          <ArrowLeftOutlined aria-hidden="true" />
          <span>{t("users.title")}</span>
        </Link>
        <span className="text-muted-foreground" aria-hidden="true">/</span>
        <span className="text-foreground font-medium">{t("users.editUser")}</span>
      </nav>

      <header className="mb-5">
        <h1 className="text-xl font-semibold tracking-tight text-foreground">{t("users.editUser")}</h1>
        <p className="mt-1 break-all text-sm text-muted-foreground">{user.email}</p>
      </header>

      {mutation.error && <div role="alert" className="mb-4 space-y-2 rounded-lg border border-destructive/30 p-3 text-sm text-destructive">
        <p>{mutation.error} {t("users.refreshAfterFailure")}</p>
        <Button variant="outline" disabled={isSaving} onClick={() => setReloadRevision(value => value + 1)}>{t("management.refresh")}</Button>
      </div>}
      <div className="grid grid-cols-1 gap-4 xl:grid-cols-[280px_minmax(0,1fr)] xl:gap-6">
        {/* Left sidebar - User profile card */}
        <aside>
          <section className="rounded-xl border bg-card p-4 sm:p-5" aria-labelledby="user-profile-heading">
            <div className="flex items-center gap-3 border-b pb-4">
              <div className="flex h-12 w-12 shrink-0 items-center justify-center rounded-lg border bg-muted text-foreground">
                <UserOutlined className="text-xl" aria-hidden="true" />
              </div>
              <div className="min-w-0">
                <h2 id="user-profile-heading" className="break-words text-base font-semibold text-foreground">
                  {user.display_name || user.email}
                </h2>
                <p className="break-all text-xs text-muted-foreground">{user.email}</p>
              </div>
            </div>

            {/* User info */}
            <dl className="divide-y">
              <div className="flex items-start gap-3 py-3">
                <MailOutlined className="mt-0.5 text-muted-foreground" aria-hidden="true" />
                <div className="min-w-0 flex-1">
                  <dt className="text-xs text-muted-foreground">{t("users.fields.email")}</dt>
                  <dd className="break-all text-sm font-medium text-foreground">{user.email}</dd>
                </div>
              </div>

              <div className="flex items-start gap-3 py-3">
                <TeamOutlined className="mt-0.5 text-muted-foreground" aria-hidden="true" />
                <div className="min-w-0 flex-1">
                  <dt className="text-xs text-muted-foreground">{t("users.fields.department")}</dt>
                  <dd className="text-sm font-medium text-foreground">{dept?.label || t("common.notSet")}</dd>
                </div>
              </div>

              <div className="flex items-start gap-3 py-3">
                <SafetyCertificateOutlined className="mt-0.5 text-muted-foreground" aria-hidden="true" />
                <div className="min-w-0 flex-1">
                  <dt className="text-xs text-muted-foreground">{t("users.fields.status")}</dt>
                  <dd>
                  <Badge
                    variant="outline"
                    className={user.status === "active"
                      ? "mt-1 border-emerald-500/30 bg-emerald-500/10 text-emerald-700 dark:text-emerald-400"
                      : "mt-1 border-destructive/30 bg-destructive/10 text-destructive"}
                  >
                    {statusOpt?.label}
                  </Badge>
                  </dd>
                </div>
              </div>

              <div className="flex items-start gap-3 py-3">
                <KeyOutlined className="mt-0.5 text-muted-foreground" aria-hidden="true" />
                <div className="min-w-0 flex-1">
                  <dt className="text-xs text-muted-foreground">{t("users.fields.roles")}</dt>
                  <dd className="mt-1 flex flex-wrap gap-1">
                    {user.roles.map((role) => (
                      <Badge key={role} variant="outline" className="bg-muted/50 text-xs text-foreground">
                        {role}
                      </Badge>
                    ))}
                  </dd>
                </div>
              </div>
            </dl>

            {/* Timestamps */}
            <div className="border-t pt-3 text-xs text-muted-foreground space-y-1">
              <p>{t("users.fields.createdAt")}: {user.created_at ? new Date(user.created_at).toLocaleString() : "-"}</p>
              <p>{t("users.fields.lastLogin")}: {user.last_login_at ? new Date(user.last_login_at).toLocaleString() : t("common.never")}</p>
            </div>
          </section>
        </aside>

        {/* Right content - Edit cards */}
        <main className="min-w-0 space-y-4 sm:space-y-5">
          {/* Basic info card */}
          <section className="rounded-xl border bg-card p-4 sm:p-5" aria-labelledby="basic-info-heading">
            <div className="mb-5 flex items-center gap-3">
              <div className="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg border bg-muted">
                <UserOutlined className="text-base text-muted-foreground" aria-hidden="true" />
              </div>
              <div className="min-w-0">
                <h2 id="basic-info-heading" className="font-semibold">{t("users.edit.basicInfo.title")}</h2>
                <p className="text-xs text-muted-foreground">{t("users.edit.basicInfo.desc")}</p>
              </div>
            </div>

            <div className="grid grid-cols-1 gap-4 sm:grid-cols-2">
              <div className="space-y-2">
                <Label htmlFor="user-edit-display-name">{t("users.fields.displayName")}</Label>
                <Input
                  id="user-edit-display-name"
                  value={formDisplayName}
                  onChange={(e) => setFormDisplayName(e.target.value)}
                  placeholder={t("users.edit.displayNamePlaceholder")}
                  disabled={!canEdit}
                />
              </div>

              <div className="space-y-2">
                <Label htmlFor="user-edit-department">{t("users.fields.department")}</Label>
                <Select value={formDepartment} onValueChange={setFormDepartment} disabled={!canEdit}>
                  <SelectTrigger id="user-edit-department">
                    <SelectValue placeholder={t("users.edit.departmentPlaceholder")} />
                  </SelectTrigger>
                  <SelectContent>
                    {departmentOptions.map((dept) => (
                      <SelectItem key={dept.value} value={dept.value}>
                        {dept.label}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>

              <div className="space-y-2">
                <Label htmlFor="user-edit-status">{t("users.fields.status")}</Label>
                <Select value={formStatus} onValueChange={setFormStatus} disabled={!canEdit}>
                  <SelectTrigger id="user-edit-status">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {statusOptions.map((opt) => (
                      <SelectItem key={opt.value} value={opt.value}>
                        {opt.label}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>
            </div>
          </section>

          {/* Roles card */}
          <section className="rounded-xl border bg-card p-4 sm:p-5" aria-labelledby="roles-heading">
            <div className="mb-5 flex items-center gap-3">
              <div className="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg border bg-muted">
                <SafetyCertificateOutlined className="text-base text-muted-foreground" aria-hidden="true" />
              </div>
              <div className="min-w-0">
                <h2 id="roles-heading" className="font-semibold">{t("users.edit.roles.title")}</h2>
                <p className="text-xs text-muted-foreground">{t("users.edit.roles.desc")}</p>
              </div>
            </div>

            {canViewRoles ? (
              <div className="grid grid-cols-1 gap-3 md:grid-cols-2">
                {roles.map((role) => {
                  const isSelected = formRoles.includes(role.role_name);
                  return (
                    <label
                      key={role.role_name}
                      htmlFor={`user-edit-role-${role.role_name}`}
                      className={`rounded-lg border p-3 transition-colors motion-reduce:transition-none sm:p-4 ${
                        isSelected ? "border-primary bg-primary/5" : "hover:bg-muted/40"
                      } ${canEdit ? "cursor-pointer" : "cursor-not-allowed opacity-60"}`}
                    >
                      <div className="flex items-start gap-3">
                        <Checkbox
                          id={`user-edit-role-${role.role_name}`}
                          checked={isSelected}
                          disabled={!canEdit}
                          className="mt-0.5"
                          onCheckedChange={(checked) => {
                            if (checked === true) {
                              setFormRoles([...formRoles, role.role_name]);
                            } else {
                              setFormRoles(formRoles.filter((item) => item !== role.role_name));
                            }
                          }}
                        />
                        <div className="flex-1 min-w-0">
                          <div className="flex items-center gap-2">
                            <span className="font-medium">{role.role_name}</span>
                            {role.is_system && (
                              <Badge variant="outline" className="text-xs">{t("users.edit.roles.system")}</Badge>
                            )}
                          </div>
                          <p className="text-xs text-muted-foreground mt-1 line-clamp-2">
                            {role.description || t("users.edit.roles.noDescription")}
                          </p>
                          <p className="text-xs text-muted-foreground mt-2">
                            {t("users.edit.roles.permissionCount", { count: role.permissions.length })}
                          </p>
                        </div>
                      </div>
                    </label>
                  );
                })}
              </div>
            ) : (
              <p className="text-sm text-muted-foreground">{t("users.edit.roles.noAccess")}</p>
            )}
          </section>

          {/* Service access policy card */}
          <section className="rounded-xl border bg-card p-4 sm:p-5" aria-labelledby="service-access-heading">
            <div className="mb-4 flex items-center gap-3">
              <div className="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg border bg-muted">
                <TeamOutlined className="text-base text-muted-foreground" aria-hidden="true" />
              </div>
              <div className="min-w-0">
                <h2 id="service-access-heading" className="font-semibold">{t("users.edit.serviceAccess.title")}</h2>
                <p className="text-xs text-muted-foreground">
                  {t("users.edit.serviceAccess.desc")}
                </p>
              </div>
            </div>

            <div className="mb-4 grid grid-cols-1 gap-4 md:grid-cols-2">
              <div className="space-y-2">
                <Label htmlFor="user-edit-service-access-mode">{t("users.edit.serviceAccess.mode")}</Label>
                <Select
                  value={serviceAccessMode}
                  onValueChange={(value) => setServiceAccessMode(value as "all" | "allowlist")}
                  disabled={!canEdit || !canViewServices}
                >
                  <SelectTrigger id="user-edit-service-access-mode">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="all">{t("users.edit.serviceAccess.allServices")}</SelectItem>
                    <SelectItem value="allowlist">{t("users.edit.serviceAccess.allowlistOnly")}</SelectItem>
                  </SelectContent>
                </Select>
              </div>
              <div className="space-y-2">
                <span className="text-sm font-medium">{t("users.edit.serviceAccess.policy")}</span>
                <div className="rounded-lg border bg-muted/40 px-3 py-2 text-xs text-muted-foreground">
                  {serviceAccessMode === "allowlist"
                    ? t("users.edit.serviceAccess.allowlistPolicy")
                    : t("users.edit.serviceAccess.allPolicy")}
                </div>
              </div>
            </div>

            {!canViewServices ? (
              <div className="text-sm text-muted-foreground">{t("users.edit.serviceAccess.noAccess")}</div>
            ) : servicesLoading ? (
              <div className="text-sm text-muted-foreground">{t("users.edit.serviceAccess.loading")}</div>
            ) : services.length === 0 ? (
              <div className="text-sm text-muted-foreground">{t("users.edit.serviceAccess.empty")}</div>
            ) : (
              <div className="grid grid-cols-1 gap-2 md:grid-cols-2">
                {services.map((svc) => {
                  const serviceId = svc.service_id;
                  const inAllowlist = formAllowedServices.includes(serviceId);
                  const inDenylist = formDeniedServices.includes(serviceId);
                  const allowDisabled = !canEdit || serviceAccessMode !== "allowlist";
                  return (
                    <div
                      key={serviceId}
                      className="rounded-lg border bg-background/50 px-3 py-3"
                    >
                      <div className="flex items-center justify-between gap-2">
                        <div className="min-w-0">
                          <p className="text-sm font-medium truncate">{svc.name || serviceId}</p>
                          <p className="text-xs text-muted-foreground truncate">{serviceId}</p>
                        </div>
                        <Badge variant="outline" className="text-[10px]">
                          {(svc.service_type || "service").toString()}
                        </Badge>
                      </div>
                      <div className="mt-3 flex items-center gap-4 text-xs">
                        <label
                          htmlFor={`allow-service-${serviceId}`}
                          className={`inline-flex items-center gap-2 ${allowDisabled ? "cursor-not-allowed" : "cursor-pointer"}`}
                        >
                          <Checkbox
                            id={`allow-service-${serviceId}`}
                            checked={inAllowlist}
                            disabled={allowDisabled}
                            onCheckedChange={() => toggleAllowedService(serviceId)}
                          />
                          <span className={allowDisabled ? "text-muted-foreground" : ""}>{t("users.edit.serviceAccess.allow")}</span>
                        </label>
                        <label
                          htmlFor={`deny-service-${serviceId}`}
                          className={`inline-flex items-center gap-2 ${canEdit ? "cursor-pointer" : "cursor-not-allowed"}`}
                        >
                          <Checkbox
                            id={`deny-service-${serviceId}`}
                            checked={inDenylist}
                            disabled={!canEdit}
                            onCheckedChange={() => toggleDeniedService(serviceId)}
                          />
                          <span className={inDenylist ? "text-destructive" : undefined}>{t("users.edit.serviceAccess.deny")}</span>
                        </label>
                      </div>
                    </div>
                  );
                })}
              </div>
            )}
          </section>

          {/* Extra permissions card */}
          <section className="rounded-xl border bg-card p-4 sm:p-5" aria-labelledby="extra-permissions-heading">
            <div className="mb-2 flex items-center gap-3">
              <div className="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg border bg-muted">
                <KeyOutlined className="text-base text-muted-foreground" aria-hidden="true" />
              </div>
              <div className="min-w-0">
                <h2 id="extra-permissions-heading" className="font-semibold">{t("users.edit.extraPermissions.title")}</h2>
                <p className="text-xs text-muted-foreground">{t("users.edit.extraPermissions.desc")}</p>
              </div>
            </div>

            <div className="mb-4 rounded-lg border bg-muted/40 p-3 text-xs text-muted-foreground">
              <strong>{t("users.edit.extraPermissions.tipLabel")}:</strong> {t("users.edit.extraPermissions.tipText")}
            </div>

            {canViewRoles && Object.keys(permissionsByCategory).length > 0 ? (
              <div className="space-y-2">
                {Object.entries(permissionsByCategory).map(([category, perms]) => {
                  const meta = categoryMeta[category] || { label: category };
                  const isExpanded = expandedCategories.has(category);
                  const selectedInCategory = perms.filter((p) => formExtraPermissions.includes(p.permission_code)).length;

                  return (
                    <div key={category} className="overflow-hidden rounded-lg border">
                      {/* Category header */}
                      <button
                        type="button"
                        id={`permission-category-trigger-${category}`}
                        aria-expanded={isExpanded}
                        aria-controls={`permission-category-panel-${category}`}
                        onClick={() => toggleCategory(category)}
                        className="flex w-full items-center justify-between gap-3 bg-muted/20 p-3 text-left transition-colors hover:bg-muted/50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring/40 motion-reduce:transition-none sm:p-4"
                      >
                        <div className="flex min-w-0 flex-wrap items-center gap-x-3 gap-y-1">
                          <span className="font-medium">{meta.label}</span>
                          <span className="text-xs text-muted-foreground">
                            ({t("users.edit.extraPermissions.categoryCount", { count: perms.length })})
                          </span>
                          {selectedInCategory > 0 && (
                            <Badge variant="default" className="text-xs">
                              +{selectedInCategory}
                            </Badge>
                          )}
                        </div>
                        <DownOutlined
                          className={`shrink-0 text-xs text-muted-foreground transition-transform motion-reduce:transition-none ${isExpanded ? "" : "-rotate-90"}`}
                          aria-hidden="true"
                        />
                      </button>

                      {/* Category content */}
                      {isExpanded && (
                        <div
                          id={`permission-category-panel-${category}`}
                          role="region"
                          aria-labelledby={`permission-category-trigger-${category}`}
                          className="border-t p-3 sm:p-4"
                        >
                          <div className="grid grid-cols-1 gap-2 md:grid-cols-2">
                              {perms.map((perm) => {
                                const fromRole = rolePermissions.has(perm.permission_code);
                                const isSelected = formExtraPermissions.includes(perm.permission_code);

                                return (
                                  <Tooltip
                                    key={perm.permission_code}
                                    title={
                                      <div>
                                        <p><strong>{t("common.code")}:</strong> {perm.permission_code}</p>
                                        {perm.description && <p><strong>{t("common.description")}:</strong> {perm.description}</p>}
                                        {fromRole && <p className="text-yellow-400 mt-1">{t("users.edit.extraPermissions.fromRole")}</p>}
                                      </div>
                                    }
                                  >
                                    <label
                                      htmlFor={`user-extra-permission-${perm.permission_code}`}
                                      className={`flex items-center gap-2 rounded-lg p-2 transition-colors motion-reduce:transition-none ${
                                        isSelected ? "bg-primary/5" : "hover:bg-muted/40"
                                      } ${canEdit && !fromRole ? "cursor-pointer" : "cursor-default opacity-60"}`}
                                    >
                                      <Checkbox
                                        id={`user-extra-permission-${perm.permission_code}`}
                                        checked={isSelected || fromRole}
                                        disabled={!canEdit || fromRole}
                                        onCheckedChange={(checked) => {
                                          if (checked === true) {
                                            setFormExtraPermissions([...formExtraPermissions, perm.permission_code]);
                                          } else {
                                            setFormExtraPermissions(
                                              formExtraPermissions.filter((item) => item !== perm.permission_code)
                                            );
                                          }
                                        }}
                                      />
                                      <div className="flex-1 min-w-0">
                                        <p className="text-sm font-medium truncate">
                                          {perm.name || perm.permission_code}
                                        </p>
                                      </div>
                                      {fromRole && (
                                        <Badge variant="outline" className="text-xs shrink-0">
                                          {t("users.edit.extraPermissions.roleBadge")}
                                        </Badge>
                                      )}
                                    </label>
                                  </Tooltip>
                                );
                              })}
                          </div>
                        </div>
                      )}
                    </div>
                  );
                })}
              </div>
            ) : canViewRoles ? (
              <p className="rounded-lg border border-dashed p-4 text-sm text-muted-foreground">
                {t("users.edit.extraPermissions.empty")}
              </p>
            ) : (
              <p className="text-sm text-muted-foreground">{t("users.edit.extraPermissions.noAccess")}</p>
            )}
          </section>
        </main>
      </div>

      {/* Fixed bottom save bar */}
      {hasChanges && (
        <div
          className="pointer-events-none fixed inset-x-0 bottom-0 z-40 px-3 pb-3 sm:px-6 sm:pb-4"
          role="status"
          aria-live="polite"
        >
          <div className="pointer-events-auto mx-auto flex max-w-3xl flex-col gap-3 rounded-xl border bg-card p-3 shadow-lg sm:flex-row sm:items-center sm:justify-between sm:p-4">
            <div className="flex items-center gap-2 text-sm">
              <ExclamationCircleOutlined className="text-amber-600 dark:text-amber-400" aria-hidden="true" />
              <span>{t("users.edit.unsavedChanges")}</span>
            </div>
            <div className="grid grid-cols-2 gap-2 sm:flex sm:items-center">
              <Button
                variant="outline"
                onClick={() => {
                  if (user) {
                    setFormDisplayName(user.display_name || "");
                    setFormDepartment(user.department || "");
                    setFormStatus(user.status);
                    setFormRoles(user.roles);
                    setFormExtraPermissions(user.extra_permissions || []);
                    setServiceAccessMode(user.service_access_mode || "all");
                    setFormAllowedServices(user.allowed_services || []);
                    setFormDeniedServices(user.denied_services || []);
                  }
                }}
              >
                {t("common.reset")}
              </Button>
              <Button
                onClick={handleSave}
                disabled={isSaving || mutation.pending || mutation.blocked || !canEdit}
                className="min-w-[100px]"
              >
                {isSaving ? (
                  <>
                    <LoadingOutlined className="mr-2" aria-hidden="true" />
                    {t("common.saving")}
                  </>
                ) : (
                  <>
                    <CheckOutlined className="mr-2" aria-hidden="true" />
                    {t("users.edit.saveChanges")}
                  </>
                )}
              </Button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
