import { getContent, getItem } from "@/api/spaces";
import { useQuery } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import { Link, useParams } from "react-router-dom";
import { KindIcon } from "./ui";

export function ItemPreview() {
  const { t } = useTranslation();
  const { itemId = "" } = useParams();
  const item = useQuery({ queryKey: ["spaces", "item", itemId], queryFn: () => getItem(itemId), enabled: itemId.length > 0 });
  const content = useQuery({
    queryKey: ["spaces", "content", itemId],
    queryFn: () => getContent(itemId),
    enabled: item.isSuccess,
  });
  return (
    <article aria-labelledby="space-item-title">
      <p><Link to="/spaces">{t("spaces.preview.back")}</Link></p>
      {item.isPending ? <p className="spaces-state">{t("spaces.preparing")}</p> : null}
      {item.isError ? (
        <div className="spaces-state" role="alert">
          <p>{t("spaces.preview.missing")}</p>
          <button type="button" onClick={() => item.refetch()}>{t("spaces.retry")}</button>
        </div>
      ) : null}
      {item.data ? (
        <>
          <h2 id="space-item-title" className="spaces-heading">
            <span className="spaces-kind"><KindIcon kind={item.data.kind} /></span>
            {item.data.title}
          </h2>
          {content.isError ? <p role="alert">{t("spaces.error")}</p> : null}
          <div className="spaces-preview">{content.data?.markdown || (content.isPending ? t("spaces.preparing") : t("spaces.preview.empty"))}</div>
        </>
      ) : null}
    </article>
  );
}
