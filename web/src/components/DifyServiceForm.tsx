import { useLayoutEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import { registerService } from "@/api/gateway";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { sdkAuthorityScope } from "@/lib/sdkAuthorityScope";
import { generateUUID } from "@/lib/utils";
import { useAuthStore } from "@/store/useAuthStore";

interface DifyServiceFormProps {
  onRegistered: () => void;
  onCancel: () => void;
  onBusyChange: (busy: boolean) => void;
}

/** Native Dify chat registration; legacy generic REST services are unchanged. */
export function DifyServiceForm(props: DifyServiceFormProps) {
  const token = useAuthStore(state => state.token);
  const authority = useAuthStore(state => sdkAuthorityScope(state.user));
  const scope = useMemo(() => ({ id: generateUUID(), token, authority }), [token, authority]);
  const isCurrent = () => {
    const state = useAuthStore.getState();
    return state.token === scope.token && sdkAuthorityScope(state.user) === scope.authority;
  };
  return <DifyServiceFields key={scope.id} {...props} isCurrent={isCurrent} />;
}

function DifyServiceFields({
  onRegistered,
  onCancel,
  onBusyChange,
  isCurrent,
}: DifyServiceFormProps & { isCurrent: () => boolean }) {
  const { t } = useTranslation();
  const [serviceId, setServiceId] = useState("");
  const [name, setName] = useState("");
  const [baseUrl, setBaseUrl] = useState("https://api.dify.ai/v1");
  const [token, setToken] = useState("");
  const [saving, setSaving] = useState(false);
  const pending = useRef(false);
  const active = useRef(true);
  const [error, setError] = useState<string | null>(null);

  const ownsView = () => active.current && isCurrent();

  useLayoutEffect(() => {
    active.current = true;
    return () => {
      active.current = false;
      onBusyChange(false);
    };
  }, [onBusyChange]);

  async function submit(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (pending.current || !ownsView()) return;
    setError(null);
    let url: URL;
    try {
      url = new URL(baseUrl.trim());
      if (
        !["https:", "http:"].includes(url.protocol) ||
        url.username || url.password || url.search || url.hash
      ) throw new Error("invalid_url");
      if (!/^[a-z0-9][a-z0-9_-]*$/.test(serviceId.trim()) || !name.trim() || !token.trim()) {
        throw new Error("invalid_fields");
      }
    } catch {
      setError(t("services.dify.invalidFields", "Enter a service ID using lowercase letters, numbers, underscores or hyphens, a name, an HTTP(S) API base URL without credentials/query/fragment, and an app API key."));
      return;
    }
    pending.current = true;
    setSaving(true);
    onBusyChange(true);
    const appKey = token.trim();
    setToken("");
    try {
      await registerService({
        service_id: serviceId.trim(),
        name: name.trim(),
        service_type: "conversational",
        supported_modes: ["sync", "stream"],
        status: "active",
        connector_type: "http",
        connector_config: {
          base_url: url.toString().replace(/\/$/, ""),
          headers: { Authorization: `Bearer ${appKey}` },
        },
        accepted_content_types: ["text"],
        output_content_types: ["text"],
        session_enabled: true,
        max_retries: 0,
        metadata: { adapter_type: "dify_chat" },
      });
      if (!ownsView()) return;
      onBusyChange(false);
      onRegistered();
    } catch {
      // Connector errors may include upstream details. Never echo app credentials.
      if (ownsView()) setError(t("services.dify.registerFailed", "Could not register the service. Check your permissions and use a unique service ID."));
    } finally {
      pending.current = false;
      if (ownsView()) {
        setSaving(false);
        onBusyChange(false);
      }
    }
  }

  return (
    <form onSubmit={submit} className="space-y-4" aria-busy={saving}>
      <p className="text-sm text-muted-foreground">
        {t("services.dify.description", "Connect a Dify chat app through the native Gateway. The app API key is saved in protected service configuration. Requires service administration permission.")}
      </p>
      <p className="text-xs text-muted-foreground">
        {t("services.dify.scope", "Text chat and streaming are supported. Agent apps require streaming. Playground resumes conversations when session persistence is enabled. Apps requiring custom inputs can use the Playground JSON editor or the API inputs parameter.")}
      </p>
      <div className="grid gap-4">
        <div className="space-y-2">
          <Label htmlFor="dify-service-id">{t("services.dify.serviceId", "Service ID")}</Label>
          <Input id="dify-service-id" value={serviceId} onChange={(e) => setServiceId(e.target.value)} placeholder="my-dify-chat" required disabled={saving} />
        </div>
        <div className="space-y-2">
          <Label htmlFor="dify-name">{t("services.dify.name", "Service name")}</Label>
          <Input id="dify-name" value={name} onChange={(e) => setName(e.target.value)} placeholder="Dify chat" required disabled={saving} />
        </div>
        <div className="space-y-2">
          <Label htmlFor="dify-base-url">{t("services.dify.baseUrl", "Application API base URL")}</Label>
          <Input id="dify-base-url" type="url" value={baseUrl} onChange={(e) => setBaseUrl(e.target.value)} placeholder="https://api.dify.ai/v1" required disabled={saving} />
        </div>
        <div className="space-y-2">
          <Label htmlFor="dify-api-key">{t("services.dify.apiKey", "Application API key")}</Label>
          <Input id="dify-api-key" type="password" autoComplete="off" value={token} onChange={(e) => setToken(e.target.value)} required disabled={saving} />
        </div>
      </div>
      {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
      <div className="flex justify-end gap-2">
        <Button type="button" variant="outline" onClick={() => { if (!pending.current && ownsView()) onCancel(); }} disabled={saving}>{t("common.cancel")}</Button>
        <Button type="submit" variant="primary" disabled={saving}>
          {saving ? t("services.registering") : t("services.register")}
        </Button>
      </div>
    </form>
  );
}
