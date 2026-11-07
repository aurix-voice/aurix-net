import "./styles.css";

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider } from "@tanstack/react-router";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { NetError } from "@/api/client";
import { AuthProvider } from "@/auth/AuthProvider";
import { I18nProvider } from "@/i18n";
import { router } from "@/router";
import { ThemeProvider } from "@/theme";
import { TooltipProvider } from "@/ui/Primitives";
import { ToastProvider } from "@/ui/Toast";

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: 5_000,
      refetchOnWindowFocus: true,
      retry: (count, err) => {
        if (err instanceof NetError && err.status >= 400 && err.status < 500) return false;
        return count < 2;
      },
    },
    mutations: { retry: false },
  },
});

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <QueryClientProvider client={queryClient}>
      <ThemeProvider>
        <I18nProvider>
          <ToastProvider>
            <TooltipProvider delayDuration={250}>
              <AuthProvider>
                <RouterProvider router={router} />
              </AuthProvider>
            </TooltipProvider>
          </ToastProvider>
        </I18nProvider>
      </ThemeProvider>
    </QueryClientProvider>
  </StrictMode>,
);
