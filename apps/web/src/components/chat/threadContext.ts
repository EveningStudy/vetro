import { createContext, useContext } from "react";

/** The working directory of the thread being rendered, for showing paths relative to it. */
export const ThreadCwdContext = createContext<string | null>(null);

export const useThreadCwd = () => useContext(ThreadCwdContext);
