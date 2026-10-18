import { lazy, Suspense } from "react";
import { Skeleton } from "antd";
import type { TableProps } from "antd";

// Fetch table layout/selection machinery only after the route has table data.
// Keep the generic record/column contract at the same typed boundary.
const Table = lazy(() => import("antd/es/table")) as unknown as typeof import("antd")["Table"];
export function DeferredTable<RecordType extends object = Record<string, unknown>>(props: TableProps<RecordType>) {
  return <Suspense fallback={<div role="status"><Skeleton active /></div>}><Table<RecordType> {...props} /></Suspense>;
}
