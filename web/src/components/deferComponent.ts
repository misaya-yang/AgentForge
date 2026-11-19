import { lazy, type ComponentType } from "react";

/** Keep independent workbench panels outside their route's static closure. */
export function deferComponent<T, K extends keyof T>(loader: () => Promise<T>, name: K) {
  return lazy(async () => ({ default: (await loader())[name] as ComponentType<object> })) as unknown as T[K];
}
