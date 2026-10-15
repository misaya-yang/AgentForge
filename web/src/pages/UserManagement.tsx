import { useState, useEffect, useMemo, useCallback, useRef } from "react";
import { Link, useNavigate } from "react-router-dom";
import { useTranslation } from "react-i18next";
import { ArrowUpDown, ArrowUp, ArrowDown, ChevronsLeft, ChevronsRight, Trash2 } from "lucide-react";
import { Button } from "@/components/ui/button";
import { getAllowedEmailDomain } from "@/config/runtime";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Badge } from "@/components/ui/badge";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
// AlertDialog removed — was causing page refresh on action click
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Checkbox } from "@/components/ui/checkbox";
import { useAuthStore } from "@/store/useAuthStore";
import {
  listUsers,
  createUser,
  updateUser,
  deleteUser,
  resetUserPassword,
  enableUser,
  disableUser,
  listRoles,
  listPermissions,
} from "@/api/users";
import type { RoleResponse, UserResponse, PermissionResponse } from "@/api/users";
import { getErrorMessage } from "@/lib/utils";
import { canAdministerAccounts, isProtectedAccount } from "@/lib/accountAdministration";
import { useAccountMutation } from "@/hooks/useAccountMutation";

export function UserManagementPage() {
  const user = useAuthStore(state => state.user);
  return <UserManagementContent key={JSON.stringify([user?.user_id, user?.roles])} />;
}

function UserManagementContent() {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const { hasPermission, user: currentUser } = useAuthStore();
  const allowedDomain = getAllowedEmailDomain();

  // Permissions
  const administrator = canAdministerAccounts(currentUser);
  const canCreate = administrator && hasPermission("user:create");
  const canEdit = administrator && hasPermission("user:edit");
  const canDelete = administrator && hasPermission("user:delete");
  const canViewRoles = hasPermission("role:list");

  // State
  const [users, setUsers] = useState<UserResponse[]>([]);
  const [roles, setRoles] = useState<RoleResponse[]>([]);
  const [permissions, setPermissions] = useState<PermissionResponse[]>([]);
  const [total, setTotal] = useState(0);
  const [page, setPage] = useState(1);
  const [pageSize, setPageSize] = useState(20);
  const [search, setSearch] = useState("");
  const [debouncedSearch, setDebouncedSearch] = useState("");
  const [statusFilter, setStatusFilter] = useState<string>("all");
  const [isLoading, setIsLoading] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);
  const directoryRequest = useRef(0);

  // Sorting state
  type SortField = "email" | "display_name" | "status" | "last_login_at";
  type SortDirection = "asc" | "desc";
  const [sortField, setSortField] = useState<SortField | null>(null);
  const [sortDirection, setSortDirection] = useState<SortDirection>("asc");

  // Page size options
  const pageSizeOptions = [10, 20, 50];

  // Modal states
  const [showCreateModal, setShowCreateModal] = useState(false);
  const [showEditModal, setShowEditModal] = useState(false);
  const [selectedUser, setSelectedUser] = useState<UserResponse | null>(null);
  const [showDeleteDialog, setShowDeleteDialog] = useState(false);
  const [showResetPasswordDialog, setShowResetPasswordDialog] = useState(false);
  const [selectedUserIds, setSelectedUserIds] = useState<Set<string>>(new Set());
  const mutation = useAccountMutation(t("users.dialog.updateFailed"));
  const isDeleting = mutation.pending;
  const [showBatchDeleteDialog, setShowBatchDeleteDialog] = useState(false);

  // Form states
  const [formEmail, setFormEmail] = useState("");
  const [formDisplayName, setFormDisplayName] = useState("");
  const [formDepartment, setFormDepartment] = useState<string>("");
  const [formRoles, setFormRoles] = useState<string[]>(["user"]);
  const [formExtraPermissions, setFormExtraPermissions] = useState<string[]>([]);
  const [formStatus, setFormStatus] = useState("active");
  const [formError, setFormError] = useState("");

  // Department options
  const departmentOptions = [
    { value: "cs", label: t("user.departments.cs") },
    { value: "sales", label: t("user.departments.sales") },
    { value: "tech", label: t("user.departments.tech") },
    { value: "admin", label: t("user.departments.admin") },
  ];

  // Load users
  const loadUsers = useCallback(async () => {
    const request = ++directoryRequest.current;
    setIsLoading(true);
    try {
      const response = await listUsers({
        page,
        page_size: pageSize,
        search: debouncedSearch || undefined,
        status: statusFilter === "all" ? undefined : statusFilter,
      });
      if (request !== directoryRequest.current) return false;
      setUsers(response.users);
      setTotal(response.total);
      setSelectedUserIds(new Set());
      return true;
    } catch (err) {
      if (request !== directoryRequest.current) return false;
      setUsers([]);
      setTotal(0);
      setActionError(getErrorMessage(err) || t("management.loadError"));
      return false;
    } finally {
      if (request === directoryRequest.current) setIsLoading(false);
    }
  }, [debouncedSearch, page, pageSize, statusFilter, t]);

  // Load roles
  const loadRoles = useCallback(async () => {
    if (!canViewRoles) {
      setRoles([]);
      return;
    }
    try {
      const response = await listRoles();
      setRoles(response.roles.filter(role => role.role_name !== "platform_admin"));
    } catch (err) {
      console.error("Failed to load roles:", err);
    }
  }, [canViewRoles]);

  // Load permissions
  const loadPermissions = useCallback(async () => {
    if (!canViewRoles) {
      setPermissions([]);
      return;
    }
    try {
      const response = await listPermissions();
      setPermissions(response.permissions);
    } catch (err) {
      console.error("Failed to load permissions:", err);
    }
  }, [canViewRoles]);

  useEffect(() => {
    loadUsers();
  }, [loadUsers]);

  // Handle sorting
  const handleSort = (field: SortField) => {
    if (sortField === field) {
      setSortDirection(sortDirection === "asc" ? "desc" : "asc");
    } else {
      setSortField(field);
      setSortDirection("asc");
    }
  };

  // Sort users client-side
  const sortedUsers = useMemo(() => {
    if (!sortField) return users;

    return [...users].sort((a, b) => {
      let aVal: string | number | null = a[sortField];
      let bVal: string | number | null = b[sortField];

      // Handle null/undefined values
      if (aVal == null) aVal = "";
      if (bVal == null) bVal = "";

      // String comparison
      if (typeof aVal === "string" && typeof bVal === "string") {
        const result = aVal.localeCompare(bVal);
        return sortDirection === "asc" ? result : -result;
      }

      return 0;
    });
  }, [users, sortField, sortDirection]);

  // Sortable header component
  const SortableHeader = ({ field, children }: { field: SortField; children: React.ReactNode }) => (
    <button
      type="button"
      className="flex items-center gap-1 rounded-sm transition-colors hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/40 motion-reduce:transition-none"
      onClick={() => handleSort(field)}
      aria-pressed={sortField === field}
    >
      {children}
      {sortField === field ? (
        sortDirection === "asc" ? (
          <ArrowUp className="h-4 w-4" aria-hidden="true" />
        ) : (
          <ArrowDown className="h-4 w-4" aria-hidden="true" />
        )
      ) : (
        <ArrowUpDown className="h-4 w-4 opacity-50" aria-hidden="true" />
      )}
    </button>
  );

  const getSortAriaValue = (field: SortField) => {
    if (sortField !== field) return "none" as const;
    return sortDirection === "asc" ? "ascending" as const : "descending" as const;
  };

  useEffect(() => {
    loadRoles();
    loadPermissions();
  }, [loadPermissions, loadRoles]);

  useEffect(() => {
    const timer = setTimeout(() => {
      setDebouncedSearch(search);
      setPage(1);
    }, 300);
    return () => clearTimeout(timer);
  }, [search]);

  const handleCreate = async () => {
    setFormError("");
    if (isLoading || actionError || !canCreate || !formEmail.trim() || !formDisplayName.trim()) {
      setFormError(t("users.dialog.requiredFields"));
      return;
    }
    if (await mutation.run(() => createUser({ email: formEmail, display_name: formDisplayName,
      department: formDepartment || undefined, roles: formRoles }))) {
      setShowCreateModal(false); resetForm(); await loadUsers();
    }
  };

  const handleUpdate = async () => {
    if (isLoading || actionError || !selectedUser || !canEdit || isProtectedAccount(selectedUser, currentUser?.user_id)) return;
    setFormError("");
    if (await mutation.run(() => updateUser(selectedUser.user_id, {
      display_name: formDisplayName, department: formDepartment || undefined,
      roles: formRoles, extra_permissions: formExtraPermissions, status: formStatus,
    }))) { setShowEditModal(false); resetForm(); await loadUsers(); }
  };

  const handleDelete = async () => {
    if (isLoading || actionError || !selectedUser || !canDelete || isProtectedAccount(selectedUser, currentUser?.user_id)) return;
    if (await mutation.run(() => deleteUser(selectedUser.user_id))) {
      setShowDeleteDialog(false); setSelectedUser(null); await loadUsers();
    }
  };

  const handleBatchDelete = async () => {
    if (isLoading || actionError || !canDelete || selectedUserIds.size === 0) return;
    const ids = Array.from(selectedUserIds);
    if (await mutation.run(async () => { for (const id of ids) await deleteUser(id); })) {
      setShowBatchDeleteDialog(false); setSelectedUserIds(new Set());
    }
    // On failure, prior successful deletions remain committed. Never replay the batch.
    await loadUsers();
  };

  // Toggle selection for a single user
  const toggleUserSelection = (userId: string) => {
    setSelectedUserIds(prev => {
      const next = new Set(prev);
      if (next.has(userId)) next.delete(userId);
      else next.add(userId);
      return next;
    });
  };

  // Select/deselect all deletable users on current page
  const toggleSelectAll = () => {
    const deletableUsers = sortedUsers.filter(
      u => !isProtectedAccount(u, currentUser?.user_id)
    );
    if (selectedUserIds.size === deletableUsers.length) {
      setSelectedUserIds(new Set());
    } else {
      setSelectedUserIds(new Set(deletableUsers.map(u => u.user_id)));
    }
  };

  const handleResetPassword = async () => {
    if (isLoading || actionError || !selectedUser || !canEdit || isProtectedAccount(selectedUser, currentUser?.user_id)) return;
    if (await mutation.run(() => resetUserPassword(selectedUser.user_id))) {
      setShowResetPasswordDialog(false); setSelectedUser(null); await loadUsers();
    }
  };

  const handleToggleStatus = async (user: UserResponse) => {
    if (isLoading || actionError || !canEdit || isProtectedAccount(user, currentUser?.user_id)) return;
    if (await mutation.run(() => user.status === "active" ? disableUser(user.user_id) : enableUser(user.user_id))) {
      await loadUsers();
    }
  };

  // Reset form
  const resetForm = () => {
    setFormEmail("");
    setFormDisplayName("");
    setFormDepartment("");
    setFormRoles(["user"]);
    setFormExtraPermissions([]);
    setFormStatus("active");
    setFormError("");
  };

  const totalPages = Math.ceil(total / pageSize);
  const isProtectedUser = (user: UserResponse) =>
    isProtectedAccount(user, currentUser?.user_id);

  const renderStatusBadge = (user: UserResponse) => (
    <Badge
      variant="outline"
      className={
        user.status === "active"
          ? "border-emerald-500/30 bg-emerald-500/10 text-emerald-700 dark:text-emerald-400"
          : "border-destructive/30 bg-destructive/10 text-destructive"
      }
    >
      <span className="flex items-center gap-1.5">
        <span
          className={`h-1.5 w-1.5 rounded-full ${
            user.status === "active" ? "bg-emerald-500" : "bg-destructive"
          }`}
          aria-hidden="true"
        />
        {user.status === "active" ? t('common.active') : t('common.disabled')}
      </span>
    </Badge>
  );

  const renderUserActions = (user: UserResponse, isMobile = false) => (
    <div className={`flex flex-wrap gap-2 ${isMobile ? "" : "justify-end"}`}>
      {canEdit && !isProtectedUser(user) && (
        <>
          <Button
            type="button"
            variant="outline"
            size="sm"
            className={isMobile ? "flex-1" : undefined}
            onClick={() => navigate(`/users/${encodeURIComponent(user.user_id)}/edit`)}
          >
            {t('users.actions.edit')}
          </Button>
          <Button
            type="button"
            variant="outline"
            size="sm"
            className={isMobile ? "flex-1" : undefined}
            onClick={() => {
              setSelectedUser(user);
              setShowResetPasswordDialog(true);
            }}
          >
            {t('users.actions.resetPassword')}
          </Button>
          {!isProtectedUser(user) && (
            <Button
              type="button"
              variant="outline"
              size="sm"
              className={isMobile ? "flex-1" : undefined}
              disabled={mutation.pending || mutation.blocked} onClick={() => handleToggleStatus(user)}
            >
              {user.status === "active" ? t('users.actions.disable') : t('users.actions.enable')}
            </Button>
          )}
        </>
      )}
      {canDelete && !isProtectedUser(user) && (
        <Button
          type="button"
          variant="destructive"
          size="sm"
          className={isMobile ? "flex-1" : undefined}
          onClick={() => {
            setSelectedUser(user);
            setShowDeleteDialog(true);
          }}
        >
          {t('users.actions.delete')}
        </Button>
      )}
    </div>
  );

  return (
    <div className="space-y-5">
      <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
        <div className="flex min-w-0 flex-col gap-3 sm:flex-row sm:items-center">
          <h1 className="text-xl font-semibold tracking-tight text-foreground">{t('users.title')}</h1>
          {canDelete && selectedUserIds.size > 0 && (
            <Button
              variant="destructive"
              size="sm"
              className="w-full sm:w-auto"
              onClick={() => setShowBatchDeleteDialog(true)}
            >
              <Trash2 className="mr-1.5 h-4 w-4" aria-hidden="true" />
              {t('users.actions.batchDelete', { count: selectedUserIds.size })}
            </Button>
          )}
        </div>
        {canCreate && (
          <Button variant="primary" className="w-full sm:w-auto" onClick={() => setShowCreateModal(true)}>
            {t('users.addUser')}
          </Button>
        )}
      </div>

      {canViewRoles && <Link to="/users/roles" className="text-sm text-primary hover:underline">{t("roleManagement.title")}</Link>}

      {actionError && <p role="alert" className="text-sm text-destructive">{actionError}</p>}
      {mutation.error && <p role="alert" className="text-sm text-destructive">{mutation.error} {t("users.refreshAfterFailure")}</p>}
      <Button variant="outline" disabled={isLoading || isDeleting} onClick={async () => {
        setActionError(null);
        if (await loadUsers()) {
          mutation.refreshed(); setSelectedUserIds(new Set()); setSelectedUser(null);
          setShowCreateModal(false); setShowEditModal(false); setShowDeleteDialog(false);
          setShowResetPasswordDialog(false); setShowBatchDeleteDialog(false); resetForm();
        }
      }}>{t("management.refresh")}</Button>
      {/* Filters */}
      <div className="grid grid-cols-1 gap-3 rounded-lg border bg-card p-3 sm:grid-cols-[minmax(0,1fr)_160px]">
        <div>
          <Label htmlFor="user-search" className="sr-only">
            {t('users.searchPlaceholder')}
          </Label>
          <Input
            id="user-search"
            type="search"
            placeholder={t('users.searchPlaceholder')}
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            className="w-full"
          />
        </div>
        <div>
          <Label htmlFor="user-status-filter" className="sr-only">
            {t('users.fields.status')}
          </Label>
          <Select
            value={statusFilter}
            onValueChange={(value) => {
              setStatusFilter(value);
              setPage(1);
            }}
          >
            <SelectTrigger id="user-status-filter" className="w-full">
              <SelectValue placeholder={t('users.allStatus')} />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="all">{t('users.allStatus')}</SelectItem>
              <SelectItem value="active">{t('common.active')}</SelectItem>
              <SelectItem value="disabled">{t('common.disabled')}</SelectItem>
            </SelectContent>
          </Select>
        </div>
      </div>

      {/* Mobile user cards */}
      <div className="space-y-3 md:hidden" aria-busy={isLoading} aria-live="polite">
        {isLoading ? (
          <div className="rounded-lg border bg-card px-4 py-10 text-center text-sm text-muted-foreground">
            {t('users.loading')}
          </div>
        ) : sortedUsers.length === 0 ? (
          <div className="rounded-lg border border-dashed bg-card px-4 py-10 text-center text-sm text-muted-foreground">
            {t('users.noUsers')}
          </div>
        ) : (
          sortedUsers.map((user) => (
            <article key={user.user_id} className="rounded-lg border bg-card p-4">
              <div className="flex items-start justify-between gap-3">
                <div className="min-w-0">
                  <h2 className="break-words text-sm font-semibold">
                    {user.display_name || user.email}
                  </h2>
                  <p className="mt-0.5 break-all text-xs text-muted-foreground">
                    {user.email}
                  </p>
                </div>
                <div className="flex shrink-0 items-center gap-3">
                  {renderStatusBadge(user)}
                  {canDelete && !isProtectedUser(user) && (
                    <Checkbox
                      checked={selectedUserIds.has(user.user_id)}
                      onCheckedChange={() => toggleUserSelection(user.user_id)}
                      aria-label={`Select ${user.email}`}
                    />
                  )}
                </div>
              </div>

              <dl className="mt-4 grid grid-cols-2 gap-x-4 gap-y-3 text-sm">
                <div className="min-w-0">
                  <dt className="text-xs text-muted-foreground">{t('users.fields.department')}</dt>
                  <dd className="mt-1 truncate">
                    {user.department
                      ? departmentOptions.find((department) => department.value === user.department)?.label || user.department
                      : t('common.notSet')}
                  </dd>
                </div>
                <div className="min-w-0">
                  <dt className="text-xs text-muted-foreground">{t('users.fields.lastLogin')}</dt>
                  <dd className="mt-1 text-xs">
                    {user.last_login_at
                      ? new Date(user.last_login_at).toLocaleString()
                      : t('common.never')}
                  </dd>
                </div>
                <div className="col-span-2 min-w-0">
                  <dt className="text-xs text-muted-foreground">{t('users.fields.roles')}</dt>
                  <dd className="mt-1.5 flex flex-wrap gap-1.5">
                    {user.roles.map((role) => (
                      <Badge key={role} variant="outline" className="bg-muted/50 font-medium text-foreground">
                        {role}
                      </Badge>
                    ))}
                  </dd>
                </div>
              </dl>

              {(canEdit || (canDelete && !isProtectedUser(user))) && (
                <div className="mt-4 border-t pt-3">
                  {renderUserActions(user, true)}
                </div>
              )}
            </article>
          ))
        )}
      </div>

      {/* Users table */}
      <div className="hidden overflow-x-auto rounded-lg border bg-card md:block" aria-busy={isLoading}>
        <Table className="min-w-[1100px]">
          <TableHeader>
            <TableRow>
              {canDelete && (
                <TableHead className="w-10">
                  <Checkbox
                    checked={sortedUsers.some(u => !isProtectedAccount(u, currentUser?.user_id)) && selectedUserIds.size === sortedUsers.filter(u => !isProtectedAccount(u, currentUser?.user_id)).length}
                    onCheckedChange={toggleSelectAll}
                    aria-label={t('users.actions.selectAll', 'Select all users')}
                  />
                </TableHead>
              )}
              <TableHead aria-sort={getSortAriaValue("email")}>
                <SortableHeader field="email">{t('users.fields.email')}</SortableHeader>
              </TableHead>
              <TableHead aria-sort={getSortAriaValue("display_name")}>
                <SortableHeader field="display_name">{t('users.fields.displayName')}</SortableHeader>
              </TableHead>
              <TableHead>{t('users.fields.department')}</TableHead>
              <TableHead>{t('users.fields.roles')}</TableHead>
              <TableHead aria-sort={getSortAriaValue("status")}>
                <SortableHeader field="status">{t('users.fields.status')}</SortableHeader>
              </TableHead>
              <TableHead aria-sort={getSortAriaValue("last_login_at")}>
                <SortableHeader field="last_login_at">{t('users.fields.lastLogin')}</SortableHeader>
              </TableHead>
              <TableHead className="text-right">{t('common.actions')}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {isLoading ? (
              <TableRow>
                <TableCell colSpan={canDelete ? 8 : 7} className="text-center py-8">
                  {t('users.loading')}
                </TableCell>
              </TableRow>
            ) : sortedUsers.length === 0 ? (
              <TableRow>
                <TableCell colSpan={canDelete ? 8 : 7} className="text-center py-8">
                  {t('users.noUsers')}
                </TableCell>
              </TableRow>
            ) : (
              sortedUsers.map((user) => (
                <TableRow key={user.user_id} className="transition-colors hover:bg-muted/40 motion-reduce:transition-none">
                  {canDelete && (
                    <TableCell>
                      {user.user_id !== currentUser?.user_id && user.user_id !== "admin" ? (
                        <Checkbox
                          checked={selectedUserIds.has(user.user_id)}
                          onCheckedChange={() => toggleUserSelection(user.user_id)}
                          aria-label={`Select ${user.email}`}
                        />
                      ) : null}
                    </TableCell>
                  )}
                  <TableCell>{user.email}</TableCell>
                  <TableCell>{user.display_name}</TableCell>
                  <TableCell>
                    {user.department ? (
                      <Badge variant="outline" className="bg-muted/50 text-foreground">
                        {departmentOptions.find(d => d.value === user.department)?.label || user.department}
                      </Badge>
                    ) : (
                      <span className="text-muted-foreground text-sm">{t('common.notSet')}</span>
                    )}
                  </TableCell>
                  <TableCell>
                    <div className="flex gap-1.5 flex-wrap">
                      {user.roles.map((role) => (
                        <Badge
                          key={role}
                          variant="outline"
                          className="bg-muted/50 font-medium text-foreground"
                        >
                          {role}
                        </Badge>
                      ))}
                    </div>
                  </TableCell>
                  <TableCell>{renderStatusBadge(user)}</TableCell>
                  <TableCell>
                    {user.last_login_at
                      ? new Date(user.last_login_at).toLocaleString()
                      : t('common.never')}
                  </TableCell>
                  <TableCell className="text-right">{renderUserActions(user)}</TableCell>
                </TableRow>
              ))
            )}
          </TableBody>
        </Table>
      </div>

      {/* Pagination */}
      <div className="flex flex-col gap-3 sm:flex-row sm:flex-wrap sm:items-center sm:justify-between">
        <div className="flex flex-col gap-1 sm:flex-row sm:items-center sm:gap-4">
          <div className="text-sm text-muted-foreground">
            {t('users.pagination.showing', {
              start: total > 0 ? (page - 1) * pageSize + 1 : 0,
              end: Math.min(page * pageSize, total),
              total: total
            })}
          </div>
          <div className="flex items-center gap-2">
            <span className="text-sm text-muted-foreground">{t('users.pagination.page', { current: page, total: Math.max(totalPages, 1) })}</span>
          </div>
        </div>
        <div className="flex flex-col gap-3 min-[420px]:flex-row min-[420px]:items-center min-[420px]:justify-between sm:justify-end sm:gap-4">
          {/* Page size selector */}
          <div className="flex items-center gap-2">
            <Label htmlFor="user-page-size" className="sr-only">
              {t('users.pagination.perPage', { count: pageSize })}
            </Label>
            <Select
              value={pageSize.toString()}
              onValueChange={(value) => {
                setPageSize(Number(value));
                setPage(1);
              }}
            >
              <SelectTrigger id="user-page-size" className="w-full min-[420px]:w-[120px]">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {pageSizeOptions.map((size) => (
                  <SelectItem key={size} value={size.toString()}>
                    {t('users.pagination.perPage', { count: size })}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
          {/* Pagination buttons */}
          <nav className="flex gap-1" aria-label={t('users.pagination.navigation', 'User list pagination')}>
            <Button
              variant="outline"
              size="icon"
              className="h-8 w-8"
              onClick={() => setPage(1)}
              disabled={page <= 1}
              title={t('users.pagination.first')}
              aria-label={t('users.pagination.first')}
            >
              <ChevronsLeft className="h-4 w-4" aria-hidden="true" />
            </Button>
            <Button
              variant="outline"
              size="sm"
              onClick={() => setPage(page - 1)}
              disabled={page <= 1}
            >
              {t('users.pagination.prev')}
            </Button>
            <Button
              variant="outline"
              size="sm"
              onClick={() => setPage(page + 1)}
              disabled={page >= totalPages}
            >
              {t('users.pagination.next')}
            </Button>
            <Button
              variant="outline"
              size="icon"
              className="h-8 w-8"
              onClick={() => setPage(totalPages)}
              disabled={page >= totalPages}
              title={t('users.pagination.last')}
              aria-label={t('users.pagination.last')}
            >
              <ChevronsRight className="h-4 w-4" aria-hidden="true" />
            </Button>
          </nav>
        </div>
      </div>

      {/* Create User Modal */}
      <Dialog open={showCreateModal} onOpenChange={open => { if (!mutation.pending) setShowCreateModal(open); }}>
        <DialogContent className="max-h-[90vh] overflow-y-auto sm:max-w-lg">
          <DialogHeader>
            <DialogTitle>{t("users.dialog.createTitle")}</DialogTitle>
            <DialogDescription>
              {t("users.dialog.createDescription")}
            </DialogDescription>
          </DialogHeader>
          <div className="space-y-4">
            <div className="space-y-2">
              <Label htmlFor="create-user-email">{t("users.fields.email")}</Label>
              <Input
                id="create-user-email"
                type="email"
                placeholder={`name@${allowedDomain}`}
                value={formEmail}
                onChange={(e) => setFormEmail(e.target.value)}
              />
            </div>
            <div className="space-y-2">
              <Label htmlFor="create-user-display-name">{t("users.fields.displayName")}</Label>
              <Input
                id="create-user-display-name"
                value={formDisplayName}
                onChange={(e) => setFormDisplayName(e.target.value)}
              />
            </div>
            <div className="space-y-2">
              <Label htmlFor="create-user-department">{t("users.fields.department")}</Label>
              <Select value={formDepartment} onValueChange={setFormDepartment}>
                <SelectTrigger id="create-user-department">
                  <SelectValue placeholder={t("users.dialog.selectDepartment")} />
                </SelectTrigger>
                <SelectContent>
                  {departmentOptions.map((dept) => (
                    <SelectItem key={dept.value} value={dept.value}>
                      {dept.label}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
              <p className="text-xs text-muted-foreground">
                {t("users.dialog.departmentHint")}
              </p>
            </div>
            <div className="space-y-2">
              <Label>{t("users.fields.roles")}</Label>
              {canViewRoles ? (
                <div className="space-y-2">
                  {roles.map((role) => (
                    <div key={role.role_name} className="flex items-center gap-2">
                      <Checkbox
                        id={`role-${role.role_name}`}
                        checked={formRoles.includes(role.role_name)}
                        onCheckedChange={(checked) => {
                          if (checked) {
                            setFormRoles([...formRoles, role.role_name]);
                          } else {
                            setFormRoles(formRoles.filter((r) => r !== role.role_name));
                          }
                        }}
                      />
                      <label htmlFor={`role-${role.role_name}`} className="text-sm">
                        {role.role_name} - {role.description}
                      </label>
                    </div>
                  ))}
                </div>
              ) : (
                <div className="text-sm text-muted-foreground">
                  {t("users.dialog.rolesNoAccessCreate")}
                </div>
              )}
            </div>
            {(formError || mutation.error) && <div role="alert" className="text-sm text-destructive">{formError || mutation.error} {mutation.error && t("users.refreshAfterFailure")}</div>}
          </div>
          <DialogFooter className="flex-col-reverse sm:flex-row">
            <Button className="w-full sm:w-auto" variant="outline" onClick={() => setShowCreateModal(false)}>
              {t("common.cancel")}
            </Button>
            <Button variant="primary" className="w-full sm:w-auto" disabled={mutation.pending || mutation.blocked} onClick={handleCreate}>{t("users.dialog.createAction")}</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* Edit User Modal */}
      <Dialog open={showEditModal} onOpenChange={open => { if (!mutation.pending) setShowEditModal(open); }}>
        <DialogContent className="max-h-[90vh] overflow-y-auto sm:max-w-lg">
          <DialogHeader>
            <DialogTitle>{t("users.dialog.editTitle")}</DialogTitle>
            <DialogDescription>
              {t("users.dialog.editDescription", { email: selectedUser?.email })}
            </DialogDescription>
          </DialogHeader>
          <div className="space-y-4">
            <div className="space-y-2">
              <Label htmlFor="edit-user-display-name">{t("users.fields.displayName")}</Label>
              <Input
                id="edit-user-display-name"
                value={formDisplayName}
                onChange={(e) => setFormDisplayName(e.target.value)}
              />
            </div>
            <div className="space-y-2">
              <Label htmlFor="edit-user-department">{t("users.fields.department")}</Label>
              <Select value={formDepartment} onValueChange={setFormDepartment}>
                <SelectTrigger id="edit-user-department">
                  <SelectValue placeholder={t("users.dialog.selectDepartment")} />
                </SelectTrigger>
                <SelectContent>
                  {departmentOptions.map((dept) => (
                    <SelectItem key={dept.value} value={dept.value}>
                      {dept.label}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
              <p className="text-xs text-muted-foreground">
                {t("users.dialog.departmentHint")}
              </p>
            </div>
            <div className="space-y-2">
              <Label htmlFor="edit-user-status">{t("users.fields.status")}</Label>
              <Select value={formStatus} onValueChange={setFormStatus}>
                <SelectTrigger id="edit-user-status">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="active">{t("users.status.active")}</SelectItem>
                  <SelectItem value="disabled">{t("users.status.disabled")}</SelectItem>
                </SelectContent>
              </Select>
            </div>
            <div className="space-y-2">
              <Label>{t("users.fields.roles")}</Label>
              {canViewRoles ? (
                <div className="space-y-2">
                  {roles.map((role) => (
                    <div key={role.role_name} className="flex items-center gap-2">
                      <Checkbox
                        id={`edit-role-${role.role_name}`}
                        checked={formRoles.includes(role.role_name)}
                        onCheckedChange={(checked) => {
                          if (checked) {
                            setFormRoles([...formRoles, role.role_name]);
                          } else {
                            setFormRoles(formRoles.filter((r) => r !== role.role_name));
                          }
                        }}
                      />
                      <label htmlFor={`edit-role-${role.role_name}`} className="text-sm">
                        {role.role_name} - {role.description}
                      </label>
                    </div>
                  ))}
                </div>
              ) : (
                <div className="text-sm text-muted-foreground">
                  {t("users.edit.roles.noAccess")}
                </div>
              )}
            </div>
            <div className="space-y-2">
              <Label>{t("users.edit.extraPermissions.title")}</Label>
              <p className="text-xs text-muted-foreground mb-2">
                {t("users.edit.extraPermissions.desc")}
              </p>
              {canViewRoles ? (
                <div className="max-h-48 overflow-y-auto border rounded-md p-3 space-y-3">
                  {/* Group permissions by category */}
                  {Object.entries(
                    permissions.reduce((acc, perm) => {
                      const cat = perm.category || "Other";
                      if (!acc[cat]) acc[cat] = [];
                      acc[cat].push(perm);
                      return acc;
                    }, {} as Record<string, PermissionResponse[]>)
                  ).map(([category, perms]) => (
                    <div key={category} className="space-y-1">
                      <div className="text-xs font-medium text-muted-foreground uppercase tracking-wide">
                        {category}
                      </div>
                      {perms.map((perm) => (
                        <div key={perm.permission_code} className="flex items-center gap-2 ml-2">
                          <Checkbox
                            id={`extra-perm-${perm.permission_code}`}
                            checked={formExtraPermissions.includes(perm.permission_code)}
                            onCheckedChange={(checked) => {
                              if (checked) {
                                setFormExtraPermissions([...formExtraPermissions, perm.permission_code]);
                              } else {
                                setFormExtraPermissions(
                                  formExtraPermissions.filter((p) => p !== perm.permission_code)
                                );
                              }
                            }}
                          />
                          <label
                            htmlFor={`extra-perm-${perm.permission_code}`}
                            className="text-sm cursor-pointer"
                            title={perm.description || perm.permission_code}
                          >
                            {perm.name || perm.permission_code}
                          </label>
                        </div>
                      ))}
                    </div>
                  ))}
                </div>
              ) : (
                <div className="text-sm text-muted-foreground">
                  {t("users.edit.extraPermissions.noAccess")}
                </div>
              )}
            </div>
            {(formError || mutation.error) && <div role="alert" className="text-sm text-destructive">{formError || mutation.error} {mutation.error && t("users.refreshAfterFailure")}</div>}
          </div>
          <DialogFooter className="flex-col-reverse sm:flex-row">
            <Button className="w-full sm:w-auto" variant="outline" onClick={() => setShowEditModal(false)}>
              {t("common.cancel")}
            </Button>
            <Button variant="primary" className="w-full sm:w-auto" disabled={mutation.pending || mutation.blocked} onClick={handleUpdate}>{t("common.save")}</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* Delete Confirmation — uses Dialog instead of AlertDialog to prevent page refresh */}
      <Dialog open={showDeleteDialog} onOpenChange={open => { if (!mutation.pending) setShowDeleteDialog(open); }}>
        <DialogContent className="sm:max-w-md">
          <DialogHeader>
            <DialogTitle>Delete User</DialogTitle>
            <DialogDescription>
              Are you sure you want to delete {selectedUser?.email}? This action cannot be undone.
            </DialogDescription>
          </DialogHeader>
          {mutation.error && <p role="alert" className="text-sm text-destructive">{mutation.error} {t("users.refreshAfterFailure")}</p>}
          <DialogFooter className="flex-col-reverse sm:flex-row">
            <Button className="w-full sm:w-auto" variant="outline" onClick={() => setShowDeleteDialog(false)} disabled={isDeleting}>Cancel</Button>
            <Button className="w-full sm:w-auto" variant="destructive" onClick={handleDelete} disabled={isDeleting || mutation.blocked}>
              {isDeleting ? "Deleting..." : "Delete"}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* Batch Delete Confirmation */}
      <Dialog open={showBatchDeleteDialog} onOpenChange={open => { if (!mutation.pending) setShowBatchDeleteDialog(open); }}>
        <DialogContent className="sm:max-w-md">
          <DialogHeader>
            <DialogTitle>Delete {selectedUserIds.size} Users</DialogTitle>
            <DialogDescription>
              Are you sure you want to delete {selectedUserIds.size} selected users? This action cannot be undone.
            </DialogDescription>
          </DialogHeader>
          {mutation.error && <p role="alert" className="text-sm text-destructive">{mutation.error} {t("users.refreshAfterFailure")}</p>}
          <DialogFooter className="flex-col-reverse sm:flex-row">
            <Button className="w-full sm:w-auto" variant="outline" onClick={() => setShowBatchDeleteDialog(false)} disabled={isDeleting}>Cancel</Button>
            <Button className="w-full sm:w-auto" variant="destructive" onClick={handleBatchDelete} disabled={isDeleting || mutation.blocked}>
              {isDeleting ? "Deleting..." : `Delete ${selectedUserIds.size} Users`}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* Reset Password Confirmation */}
      <Dialog open={showResetPasswordDialog} onOpenChange={open => { if (!mutation.pending) setShowResetPasswordDialog(open); }}>
        <DialogContent className="sm:max-w-md">
          <DialogHeader>
            <DialogTitle>Reset Password</DialogTitle>
            <DialogDescription>
              Reset password for {selectedUser?.email} to the configured default? The user will be required to change password on next login.
            </DialogDescription>
          </DialogHeader>
          {mutation.error && <p role="alert" className="text-sm text-destructive">{mutation.error} {t("users.refreshAfterFailure")}</p>}
          <DialogFooter className="flex-col-reverse sm:flex-row">
            <Button className="w-full sm:w-auto" variant="outline" onClick={() => setShowResetPasswordDialog(false)}>Cancel</Button>
            <Button variant="primary" className="w-full sm:w-auto" onClick={handleResetPassword} disabled={mutation.pending || mutation.blocked}>Reset Password</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
