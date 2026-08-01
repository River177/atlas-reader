import type { AtlasError } from "@atlas/contracts";

export function errorMessage(reason: unknown): string {
  if (reason instanceof Error) {
    return reason.message;
  }
  if (isAtlasError(reason)) {
    return reason.message;
  }
  if (typeof reason === "string") {
    return reason;
  }
  return "Atlas Reader could not complete the operation";
}

function isAtlasError(reason: unknown): reason is AtlasError {
  return (
    typeof reason === "object" &&
    reason !== null &&
    "message" in reason &&
    typeof reason.message === "string"
  );
}
