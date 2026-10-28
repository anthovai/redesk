-- LAN facts reported by agents at registration, used for Wake-on-LAN.
ALTER TABLE devices ADD COLUMN macs TEXT[] NOT NULL DEFAULT '{}';
ALTER TABLE devices ADD COLUMN lan JSONB NOT NULL DEFAULT '[]';
