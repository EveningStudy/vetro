import { create } from "zustand";
import { RpcFailure } from "~/api/protocol";

export interface Toast {
  id: number;
  kind: "error" | "info" | "success";
  text: string;
}

export const useToasts = create<{ toasts: Toast[] }>(() => ({ toasts: [] }));

let nextId = 1;

function push(kind: Toast["kind"], text: string) {
  const id = nextId++;
  useToasts.setState((state) => ({ toasts: [...state.toasts.slice(-3), { id, kind, text }] }));
  setTimeout(() => dismiss(id), kind === "error" ? 6000 : 3000);
}

export function dismiss(id: number) {
  useToasts.setState((state) => ({ toasts: state.toasts.filter((toast) => toast.id !== id) }));
}

export const toast = {
  error: (text: string) => push("error", text),
  info: (text: string) => push("info", text),
  success: (text: string) => push("success", text),
};

export function errorMessage(error: unknown) {
  if (error instanceof RpcFailure) return error.message;
  if (error instanceof Error) return error.message;
  return String(error);
}
