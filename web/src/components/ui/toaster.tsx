import { lazy, Suspense } from "react"
import { useToast } from "@/hooks/use-toast"

const ToastList = lazy(async () => {
  const module = await import("./toast-list")
  return { default: module.ToastList }
})

export function Toaster() {
  const { toasts } = useToast()

  // Keep the store subscribed, but load the toast UI only when it is needed.
  if (toasts.length === 0) return null

  return (
    <Suspense fallback={null}>
      <ToastList toasts={toasts} />
    </Suspense>
  )
}
