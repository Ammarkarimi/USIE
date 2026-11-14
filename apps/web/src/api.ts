import { UsieClient } from "@usie/sdk";

/** The single API client of the application (cookie session + CSRF). */
export const api = new UsieClient();
