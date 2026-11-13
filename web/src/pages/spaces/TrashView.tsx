import { listTrash, restoreItem, type SpaceItem } from "@/api/spaces";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import { relativeTime } from "./format";
import { KindIcon } from "./ui";

export function TrashView() {
  const { t, i18n } = useTranslation();
  const client = useQueryClient();
  const trash = useQuery({ queryKey: ["spaces", "trash"], queryFn: () => listTrash({ limit: 50 }) });
  const restore = useMutation({
    mutationFn: (item: SpaceItem) => restoreItem(item.item_id),
    onSuccess: () => client.invalidateQueries({ queryKey: ["spaces"] }),
  });
  const rows = trash.data?.items ?? [];
  return (
    <section aria-labelledby="spaces-trash-heading">
      <h2 id="spaces-trash-heading" className="spaces-heading">{t("spaces.trash")}</h2>
      {trash.isPending ? <p className="spaces-state">{t("spaces.preparing")}</p> : null}
      {trash.isError ? (
        <div className="spaces-state" role="alert">
          <p>{t("spaces.error")}</p>
          <button type="button" onClick={() => trash.refetch()}>{t("spaces.retry")}</button>
        </div>
      ) : null}
      {!trash.isPending && rows.length === 0 ? <p className="spaces-state">{t("spaces.emptyTrash")}</p> : null}
      {rows.length > 0 ? (
        <table className="spaces-table">
          <caption className="sr-only">{t("spaces.itemCount", { count: rows.length })}</caption>
          <thead>
            <tr>
              <th scope="col">{t("spaces.columns.name")}</th>
              <th scope="col">{t("spaces.columns.activity")}</th>
              <th scope="col">{t("spaces.actions.restore")}</th>
            </tr>
          </thead>
          <tbody>
            {rows.map((item) => (
              <tr key={item.item_id} className="spaces-row">
                <td>
                  <span className="spaces-name">
                    <span className="spaces-kind"><KindIcon kind={item.kind} /></span>
                    <span className="spaces-title">{item.title}</span>
                  </span>
                </td>
                <td className="spaces-muted">
                  <time dateTime={item.deleted_at ?? item.updated_at}>{relativeTime(item.deleted_at ?? item.updated_at, Date.now(), i18n.language)}</time>
                </td>
                <td>
                  <button type="button" className="spaces-text-button" onClick={() => restore.mutate(item)}>{t("spaces.actions.restore")}</button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      ) : null}
    </section>
  );
}
