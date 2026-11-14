import { lazy, Suspense } from "react";
import type { SelectProps } from "antd";
const Select = lazy(() => import("antd/es/select")) as unknown as typeof import("antd")["Select"];
/** Selection machinery loads only for mounted controls; inactive workbench panels stay cheap. */
export function DeferredSelect<T = string>(props: SelectProps<T>) {
  return <Suspense fallback={<span className="forge-select-skeleton" role="progressbar" aria-label={props["aria-label"]} />}><Select {...props} /></Suspense>;
}
