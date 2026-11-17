// Errors from the Rust API client, and the user-facing wording for them.
// Wording only: which request is valid, and why, is decided by Rust.

export type ApiErrorKind =
  /** Orchestrator could not be reached (connection refused, DNS, TLS, ...). */
  | "network"
  /** No response within the configured client timeout. */
  | "timeout"
  /** Orchestrator answered with a non-2xx status. `code` is its stable error code. */
  | "http"
  /** 2xx response whose body did not match the expected contract. */
  | "invalid_response";

export class ApiError extends Error {
  readonly kind: ApiErrorKind;
  readonly status: number;
  readonly code: string;

  constructor(kind: ApiErrorKind, status: number, code: string, message: string) {
    super(message);
    this.name = "ApiError";
    this.kind = kind;
    this.status = status;
    this.code = code;
  }

  /** The request may have reached Rust and succeeded; retrying needs care. */
  get uncertain(): boolean {
    return this.kind === "timeout" || (this.kind === "http" && this.status >= 500);
  }
}

export interface ErrorCopy {
  title: string;
  detail: string;
}

/** Copy for an error that prevented a page from loading its data. */
export function describeLoadError(err: unknown, subject: string): ErrorCopy {
  if (err instanceof ApiError) {
    switch (err.kind) {
      case "network":
        return {
          title: "The orchestrator API is unreachable",
          detail: `We couldn't load ${subject} because the Rust API did not respond. Check that it is running, then try again.`,
        };
      case "timeout":
        return {
          title: "The orchestrator API took too long to respond",
          detail: `Loading ${subject} timed out. Try again in a moment.`,
        };
      case "invalid_response":
        return {
          title: "Unexpected response from the API",
          detail: `The API returned ${subject} in a format this portal does not understand. The portal and API versions may be out of sync.`,
        };
      case "http":
        if (err.status === 503) {
          return {
            title: "The API is temporarily unavailable",
            detail: `The API could not reach its database while loading ${subject}. Try again shortly.`,
          };
        }
        return {
          title: `Couldn't load ${subject}`,
          detail: `The API responded with an error (HTTP ${err.status}, ${err.code}).`,
        };
    }
  }
  return { title: `Couldn't load ${subject}`, detail: "An unexpected error occurred. Try again." };
}

/** Turns Rust's `invalid field \`question\`: reason` into a sentence for a field. */
export function fieldMessage(message: string): string {
  const reason = message.replace(/^invalid field `[^`]*`:\s*/, "");
  return reason.charAt(0).toUpperCase() + reason.slice(1).replace(/\.?$/, ".");
}
