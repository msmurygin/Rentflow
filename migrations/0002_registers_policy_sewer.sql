-- Spec v0.3: tariff registers (T1/T2), per-register precision policy, explicit sewer service,
-- baselines with an unconfirmed date, raw readings kept beside billable ones.

CREATE TABLE meter_register (
    id            uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    meter_id      uuid NOT NULL REFERENCES meter(id),
    register_code text NOT NULL,           -- 'main', 'T1', 'T2'...
    display_label text NOT NULL,           -- shown to people; not tied to time of day
    unit          text NOT NULL,
    -- how a cumulative reading becomes a billable value; a register/contract setting, not global
    precision     text NOT NULL DEFAULT 'exact' CHECK (precision IN ('exact', 'floor_before_delta')),
    active        boolean NOT NULL DEFAULT true,
    UNIQUE (meter_id, register_code)
);

-- readings belong to a register
ALTER TABLE meter_reading DROP COLUMN meter_id;
ALTER TABLE meter_reading ADD COLUMN meter_register_id uuid NOT NULL REFERENCES meter_register(id);
-- a baseline may have an unconfirmed date (NULL); it is never replaced by a guessed one
ALTER TABLE meter_reading ALTER COLUMN read_at DROP NOT NULL;
ALTER TABLE meter_reading ADD CONSTRAINT meter_reading_date_required_unless_baseline
    CHECK (read_at IS NOT NULL OR source = 'initial');
CREATE INDEX meter_reading_register_read_at ON meter_reading (meter_register_id, read_at);

-- tariffs: per register, or the property-level sewer service
ALTER TABLE tariff DROP COLUMN meter_id;
ALTER TABLE tariff ADD COLUMN meter_register_id uuid REFERENCES meter_register(id);
ALTER TABLE tariff ADD COLUMN property_id uuid REFERENCES property(id);
ALTER TABLE tariff ADD COLUMN service text CHECK (service IN ('sewer'));
ALTER TABLE tariff ADD CONSTRAINT tariff_register_xor_service
    CHECK ((meter_register_id IS NOT NULL) <> (service IS NOT NULL));
ALTER TABLE tariff ADD CONSTRAINT tariff_service_needs_property
    CHECK (service IS NULL OR property_id IS NOT NULL);

-- sewer is charged to the tenant only by explicit choice
ALTER TABLE tenancy ADD COLUMN sewer_billed boolean NOT NULL DEFAULT false;

-- bill lines: per register (or the derived sewer line), with raw values beside billable ones
ALTER TABLE bill_line DROP COLUMN meter_id;
ALTER TABLE bill_line ADD COLUMN meter_register_id uuid REFERENCES meter_register(id);
ALTER TABLE bill_line ADD COLUMN kind text NOT NULL DEFAULT 'register' CHECK (kind IN ('register', 'sewer'));
ALTER TABLE bill_line ADD COLUMN previous_raw numeric(14,3);
ALTER TABLE bill_line ADD COLUMN current_raw numeric(14,3);
ALTER TABLE bill_line ADD CONSTRAINT bill_line_register_iff_register_kind
    CHECK ((kind = 'register') = (meter_register_id IS NOT NULL));
