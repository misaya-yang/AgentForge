import { useLayoutEffect, useRef, useState } from "react";
import { message } from "antd";
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
import { changePassword } from "@/api/auth";
import { useAuthStore } from "@/store/useAuthStore";

import { useAuthenticatedReadScope } from "@/hooks/useAuthenticatedReadScope";

interface PasswordChangeModalProps {
  open: boolean;
  onComplete: () => void;
  allowClose?: boolean;
  onClose?: () => void;
}

export function PasswordChangeModal(props: PasswordChangeModalProps) {
  const scope = useAuthenticatedReadScope();
  // Closing or replacing authority unmounts all in-memory credential fields.
  if (!props.open || !scope.isCurrent()) return null;
  return <PasswordChangeForm key={scope.id} {...props} isCurrent={scope.isCurrent} />;
}

function PasswordChangeForm({
  open,
  onComplete,
  allowClose = false,
  onClose,
  isCurrent,
}: PasswordChangeModalProps & { isCurrent: () => boolean }) {
  const { clearAuth } = useAuthStore();
  const { t } = useTranslation();

  const [currentPassword, setCurrentPassword] = useState("");
  const [newPassword, setNewPassword] = useState("");
  const [confirmPassword, setConfirmPassword] = useState("");
  const [error, setError] = useState("");
  const [isLoading, setIsLoading] = useState(false);
  const pending = useRef(false);
  const blocked = useRef(false);
  const [uncertain, setUncertain] = useState(false);
  const mounted = useRef(true);
  useLayoutEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, []);
  const ownsAttempt = () => mounted.current && isCurrent();
  const close = () => {
    if (!allowClose || pending.current || blocked.current || !ownsAttempt()) return;
    mounted.current = false;
    setCurrentPassword("");
    setNewPassword("");
    setConfirmPassword("");
    onClose?.();
  };

  const validatePassword = (password: string): string[] => {
    const errors: string[] = [];
    if (password.length < 8) {
      errors.push("At least 8 characters");
    }
    if (!/[a-zA-Z]/.test(password)) {
      errors.push("At least one letter");
    }
    if (!/\d/.test(password)) {
      errors.push("At least one number");
    }
    if (!/[!@#$%^&*(),.?":{}|<>\-_=+[\]\\;'`~]/.test(password)) {
      errors.push("At least one special character");
    }
    return errors;
  };

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!ownsAttempt() || pending.current || blocked.current) return;
    setError("");

    // Validate new password
    const validationErrors = validatePassword(newPassword);
    if (validationErrors.length > 0) {
      setError(`Password requirements: ${validationErrors.join(", ")}`);
      return;
    }

    // Check passwords match
    if (newPassword !== confirmPassword) {
      setError("Passwords do not match");
      return;
    }

    // Check new password is different
    if (newPassword === currentPassword) {
      setError("New password must be different from current password");
      return;
    }

    pending.current = true;
    setIsLoading(true);

    try {
      await changePassword({
        current_password: currentPassword,
        new_password: newPassword,
        confirm_password: confirmPassword,
      });

      if (!ownsAttempt()) return;

      // A password change invalidates all old JWT sessions, including this one.
      void message.success(t("auth.passwordChangedSignIn", "Password changed. Sign in again."));
      clearAuth();

      // Clear form
      setCurrentPassword("");
      setNewPassword("");
      setConfirmPassword("");

      onComplete();
    } catch (err: unknown) {
      if (!ownsAttempt()) return;
      const axiosError = err as { response?: { status?: number; data?: { detail?: string } } };
      const status = axiosError?.response?.status;
      if (typeof status !== "number" || status < 400 || status >= 500 || status === 408) {
        // A lost acknowledgement is not evidence the password stayed unchanged.
        blocked.current = true;
        setUncertain(true);
        setCurrentPassword("");
        setNewPassword("");
        setConfirmPassword("");
        setError(t("auth.passwordChangeUnknown"));
      } else {
        setError(axiosError.response?.data?.detail || "Failed to change password");
      }
    } finally {
      pending.current = false;
      if (ownsAttempt()) setIsLoading(false);
    }
  };

  return (
    <Dialog
      open={open}
      onOpenChange={(nextOpen) => {
        if (!nextOpen) close();
      }}
    >
      <DialogContent
        className="sm:max-w-md"
        showCloseButton={allowClose && !isLoading && !uncertain}
        onEscapeKeyDown={(e) => {
          if (!allowClose || pending.current || blocked.current) e.preventDefault();
        }}
        onInteractOutside={(e) => {
          if (!allowClose || pending.current || blocked.current) {
            e.preventDefault();
          }
        }}
      >
        <DialogHeader>
          <DialogTitle>Change Password</DialogTitle>
          <DialogDescription>
            You must change your password before continuing.
          </DialogDescription>
        </DialogHeader>

        <form onSubmit={handleSubmit} className="space-y-4">
          <div className="space-y-2">
            <Label htmlFor="current-password">Current Password</Label>
            <Input
              id="current-password"
              type="password"
              value={currentPassword}
              onChange={(e) => setCurrentPassword(e.target.value)}
              required
              disabled={isLoading || uncertain}
            />
          </div>

          <div className="space-y-2">
            <Label htmlFor="new-password">New Password</Label>
            <Input
              id="new-password"
              type="password"
              value={newPassword}
              onChange={(e) => setNewPassword(e.target.value)}
              required
              disabled={isLoading || uncertain}
            />
            <p className="text-xs text-muted-foreground">
              Minimum 8 characters with letter, number, and special character
            </p>
          </div>

          <div className="space-y-2">
            <Label htmlFor="confirm-password">Confirm New Password</Label>
            <Input
              id="confirm-password"
              type="password"
              value={confirmPassword}
              onChange={(e) => setConfirmPassword(e.target.value)}
              required
              disabled={isLoading || uncertain}
            />
          </div>

          {error && (
            <div className="text-sm text-red-500">{error}</div>
          )}

          <DialogFooter>
            {allowClose && (
              <Button type="button" variant="outline" disabled={isLoading || uncertain} onClick={close}>
                Cancel
              </Button>
            )}
            {uncertain ? <Button type="button" onClick={() => {
              if (!ownsAttempt()) return;
              clearAuth();
              onComplete();
            }}>{t("auth.passwordChangeSignIn")}</Button> : <Button
              type="submit"
              className={allowClose ? "" : "w-full"}
              disabled={isLoading || uncertain}
            >
              {isLoading ? "Changing Password..." : "Change Password"}
            </Button>}
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
