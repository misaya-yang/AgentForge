import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { BrowserRouter } from "react-router-dom";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { AxiosError } from "axios";
import "./index.css";
import "./styles/tokens.css";
import "./styles/themes.css";
import { initializeI18n } from "./i18n";
import App from "./App";
import { RootErrorBoundary } from "@/components/RootErrorBoundary";
import { initInteractionTelemetry } from "@/features/chat/telemetry";

// Custom retry logic for rate limit (429) errors
const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      // Retry configuration
      retry: (failureCount, error) => {
        // Don't retry on 4xx errors except 429 (rate limit)
        if (error instanceof AxiosError) {
          const status = error.response?.status;
          // 429: retry with backoff
          if (status === 429) {
            return failureCount < 3; // Max 3 retries for rate limit
          }
          // Don't retry other 4xx errors
          if (status && status >= 400 && status < 500) {
            return false;
          }
        }
        // Default: retry up to 3 times for network/server errors
        return failureCount < 3;
      },
      // Delay between retries (respects Retry-After for 429)
      retryDelay: (attemptIndex, error) => {
        // For 429 errors, use Retry-After header if available
        if (error instanceof AxiosError && error.response?.status === 429) {
          const retryAfter =
            (error as AxiosError & { retryAfter?: number }).retryAfter ||
            parseInt(error.response.headers["retry-after"] || "5", 10) * 1000;
          // Add some jitter to prevent thundering herd
          return retryAfter + Math.random() * 1000;
        }
        // Exponential backoff for other errors: 1s, 2s, 4s...
        return Math.min(1000 * 2 ** attemptIndex, 30000);
      },
      // Stale time: consider data fresh for 30 seconds
      staleTime: 30 * 1000,
      // Don't refetch on window focus during rate limit recovery
      refetchOnWindowFocus: false,
    },
    mutations: {
      // Similar retry logic for mutations
      retry: (failureCount, error) => {
        if (error instanceof AxiosError) {
          const status = error.response?.status;
          if (status === 429) {
            return failureCount < 2; // Fewer retries for mutations
          }
          if (status && status >= 400 && status < 500) {
            return false;
          }
        }
        return failureCount < 2;
      },
    },
  },
});

async function bootstrap() {
  await initializeI18n();
  initInteractionTelemetry();
  createRoot(document.getElementById("root")!).render(
    <StrictMode>
      <RootErrorBoundary>
        <QueryClientProvider client={queryClient}>
          {/* v7: the old v7_startTransition / v7_relativeSplatPath future flags are now default behavior. */}
          <BrowserRouter>
            <App />
          </BrowserRouter>
        </QueryClientProvider>
      </RootErrorBoundary>
    </StrictMode>
  );
}

void bootstrap();
