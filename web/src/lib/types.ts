export type Role = 'landlord' | 'tenant' | 'new';
export type BillStatus = 'draft' | 'approved' | 'paid' | 'rejected' | 'void';

export interface PropertyRef {
  id: number;
  name: string;
  role: 'owner' | 'tenant';
  active: boolean;
}
export interface Me {
  role: Role;
  name: string;
  bot_username: string;
  property_configured: boolean;
  properties: PropertyRef[];
}
export interface Register {
  code: string;
  label: string;
  unit: string;
  baseline: string;
  baseline_date: string | null;
  last: { value: string; read_on: string | null; period: string } | null;
}
export interface Property {
  id: number;
  name: string;
  tenant_name: string;
  tenant_connected: boolean;
  reading_day: number | null;
  proof_required?: boolean;
  registers: Register[];
}
export interface BillSummary {
  id: number;
  period: string;
  version: number;
  status: BillStatus;
  total: string;
  created_at: string;
  decided_at: string | null;
  paid_at: string | null;
}
export interface Home {
  role: Role;
  property: { name: string; tenant_name: string; tenant_connected: boolean } | null;
  period?: string;
  received?: number;
  total_registers?: number;
  billed?: boolean;
  next_due?: string | null;
  due_date?: string | null;
  last_bill?: { id: number; period: string; status: BillStatus; total: string } | null;
}
export interface MeterRow {
  code: string;
  label: string;
  unit: string;
  previous: { value: string; read_on: string | null; period: string } | null;
  current: string | null;
}
export interface FormRegister {
  code: string;
  label: string;
  unit: string;
  previous: string | null;
  value: string | null;
  has_photo: boolean;
  source: 'manual' | 'photo' | null;
  ai_value: string | null;
  ai_confidence: string | null;
}
export interface FormView {
  period: string;
  billed: boolean;
  proof_required: boolean;
  ocr_enabled: boolean;
  registers: FormRegister[];
  complete: boolean;
}
export interface TariffVersion {
  month: string;
  price: string;
  enabled: boolean;
}
export interface TariffItem {
  code: string;
  label: string;
  unit: string;
  effective: TariffVersion | null;
  versions: TariffVersion[];
}
export interface Tariffs {
  month: string;
  items: TariffItem[];
  draft_id?: number | null;
}
export interface SnapLine {
  code: string;
  previous_raw: string;
  current_raw: string;
  previous: string;
  current: string;
  usage: string;
  price: string;
  amount: string;
  previous_read_on: string | null;
}
export interface BillDetail extends BillSummary {
  snapshot: { period: string; currency: string; total: string; policy: string; lines: SnapLine[] };
  readings: { reading_id: number; code: string; value: string; read_on: string | null; has_photo: boolean }[];
  can_approve: boolean;
  can_reject: boolean;
  can_mark_paid: boolean;
  can_reissue: boolean;
  can_void: boolean;
}
