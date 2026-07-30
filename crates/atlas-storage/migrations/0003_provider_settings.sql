ALTER TABLE provider_profiles
  ADD COLUMN endpoint_fingerprint TEXT NOT NULL DEFAULT '';

ALTER TABLE provider_profiles
  ADD COLUMN adapter_protocol_version TEXT NOT NULL DEFAULT '';
