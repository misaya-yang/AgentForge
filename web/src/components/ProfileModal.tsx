import { useState, useEffect } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { useAuthStore } from "@/store/useAuthStore";
import { api } from "@/lib/api";
import { Badge } from "@/components/ui/badge";

interface ProfileModalProps {
  open: boolean;
  onClose: () => void;
}

// Department display mapping
export function ProfileModal({ open, onClose }: ProfileModalProps) {
  const { t } = useTranslation();
  const { user, updateUser } = useAuthStore();

  const departmentLabels: Record<string, string> = {
    cs: t("user.departments.cs"),
    sales: t("user.departments.sales"),
    tech: t("user.departments.tech"),
    admin: t("user.departments.admin"),
  };

  const [displayName, setDisplayName] = useState("");
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState("");
  const [success, setSuccess] = useState("");

  // Initialize form when modal opens
  useEffect(() => {
    if (open && user) {
      setDisplayName(user.display_name || "");
      setError("");
      setSuccess("");
    }
  }, [open, user]);

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    setError("");
    setSuccess("");
    setIsLoading(true);

    try {
      // Update profile via API
      await api.put(`/api/v1/users/${user?.user_id}/profile`, {
        display_name: displayName.trim(),
      });

      // Update local state
      updateUser({ display_name: displayName.trim() });
      setSuccess(t("user.profileUpdateSuccess"));

      // Close after short delay
      setTimeout(() => {
        onClose();
      }, 1000);
    } catch (err: unknown) {
      const axiosError = err as { response?: { data?: { detail?: string } } };
      setError(axiosError.response?.data?.detail || t("user.profileUpdateFailed"));
    } finally {
      setIsLoading(false);
    }
  };

  if (!user) return null;

  return (
    <Dialog open={open} onOpenChange={(nextOpen) => !nextOpen && onClose()}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>{t("user.profileSettings")}</DialogTitle>
          <DialogDescription>
            {t("user.profileSettingsDesc")}
          </DialogDescription>
        </DialogHeader>

        <form onSubmit={handleSubmit} className="space-y-4">
          {/* Email - Read only */}
          <div className="space-y-2">
            <Label className="text-muted-foreground">{t("user.email")}</Label>
            <Input
              value={user.email || ""}
              disabled
              className="bg-muted"
            />
            <p className="text-xs text-muted-foreground">{t("user.emailReadonly")}</p>
          </div>

          {/* Display Name - Editable */}
          <div className="space-y-2">
            <Label htmlFor="displayName">{t("user.displayName")}</Label>
            <Input
              id="displayName"
              value={displayName}
              onChange={(e) => setDisplayName(e.target.value)}
              placeholder={t("user.displayNamePlaceholder")}
              disabled={isLoading}
            />
          </div>

          {/* Department - Read only, assigned by admin */}
          <div className="space-y-2">
            <Label className="text-muted-foreground">{t("user.department")}</Label>
            <div className="flex items-center gap-2 p-2 bg-muted rounded-md min-h-[40px]">
              {user.department ? (
                <Badge variant="secondary">
                  {departmentLabels[user.department] || user.department}
                </Badge>
              ) : (
                <span className="text-sm text-muted-foreground">{t("user.departmentUnassigned")}</span>
              )}
            </div>
            <p className="text-xs text-muted-foreground">
              {t("user.departmentHint")}
            </p>
          </div>

          {/* Roles - Read only */}
          <div className="space-y-2">
            <Label className="text-muted-foreground">{t("user.roles")}</Label>
            <div className="flex flex-wrap gap-2 p-2 bg-muted rounded-md min-h-[40px]">
              {user.roles.map((role) => (
                <Badge key={role} variant="outline">
                  {role}
                </Badge>
              ))}
            </div>
          </div>

          {error && (
            <div className="text-sm text-red-500">{error}</div>
          )}
          {success && (
            <div className="text-sm text-green-500">{success}</div>
          )}

          <DialogFooter>
            <Button type="button" variant="outline" onClick={onClose}>
              {t("common.cancel")}
            </Button>
            <Button type="submit" disabled={isLoading}>
              {isLoading ? t("common.saving") : t("common.save")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
