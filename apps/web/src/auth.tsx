import { type ReactNode, createContext, useCallback, useContext, useEffect, useState } from "react";
import { ApiError, type Principal, type Role } from "@usie/sdk";
import { api } from "./api";

interface AuthState {
  principal: Principal | null;
  loading: boolean;
  setPrincipal: (p: Principal | null) => void;
  logout: () => Promise<void>;
}

const AuthContext = createContext<AuthState | null>(null);

export function AuthProvider({ children }: { children: ReactNode }) {
  const [principal, setPrincipal] = useState<Principal | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<unknown>(null);
  useEffect(() => {
    api.me().then(
      (s) => setPrincipal(s.principal),
      (e: unknown) => {
        if (!(e instanceof ApiError && e.status === 401)) setError(e);
      },
    ).finally(() => setLoading(false));
  }, []);
  const logout = useCallback(async () => {
    await api.logout();
    setPrincipal(null);
  }, []);
  if (error) {
    const msg = error instanceof Error ? error.message : String(error);
    return (
      <div role="alert" className="m-8 rounded-md border border-danger/40 bg-danger/10 p-4 text-danger">
        The USIE API is not reachable: {msg}
      </div>
    );
  }
  return <AuthContext.Provider value={{ principal, loading, setPrincipal, logout }}>{children}</AuthContext.Provider>;
}

export function useAuth(): AuthState {
  const ctx = useContext(AuthContext);
  if (!ctx) throw new Error("useAuth outside AuthProvider");
  return ctx;
}

const rank: Record<Role, number> = { viewer: 1, member: 2, admin: 3, owner: 4 };

/** Whether the signed-in user's role allows an action. The server enforces this too. */
export function useCan(min: Role): boolean {
  const { principal } = useAuth();
  return principal ? rank[principal.role] >= rank[min] : false;
}
