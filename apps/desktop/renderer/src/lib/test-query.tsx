import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { ReactNode } from "react";

import { ReviewLiveSync } from "@/components/review/review-live-sync";

/**
 * `lib/test-query` — the TanStack Query test harness for the review hooks (PDR-0029). Each call
 * returns a FRESH, isolated `QueryClient` (no cache leak between tests) plus a `wrapper` that mounts
 * `<QueryClientProvider>` AND the real `<ReviewLiveSync>` — so a test that `emit("repos://changed")`
 * drives the SAME single-subscription invalidation the app uses (the "agent reply appears live" /
 * "unread rises" / "living diff re-pulls" paths run through the production code, not a test-only
 * shortcut).
 *
 * Retries are off and `gcTime: Infinity` keeps the deterministic single-run behaviour tests expect.
 */
export function createQueryWrapper(): {
  wrapper: (props: { children: ReactNode }) => ReactNode;
  queryClient: QueryClient;
} {
  const queryClient = new QueryClient({
    defaultOptions: {
      queries: { retry: false, refetchOnWindowFocus: false, gcTime: Number.POSITIVE_INFINITY },
      mutations: { retry: false },
    },
  });
  const wrapper = ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={queryClient}>
      <ReviewLiveSync />
      {children}
    </QueryClientProvider>
  );
  return { wrapper, queryClient };
}
