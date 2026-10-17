import { describe, expect, it } from 'vitest';
import { addMonth, dateLong, isValidNumber, money, monthFrom, monthName, num, price, usage } from './format';

describe('format', () => {
  it('formats money from decimal strings without floats', () => {
    expect(money('2121.21')).toBe('2 121,21 ₽');
    expect(money('65.7')).toBe('65,70 ₽');
    expect(money('0')).toBe('0,00 ₽');
    expect(money('1234567.89')).toBe('1 234 567,89 ₽');
    expect(money(null)).toBe('—');
  });
  it('formats readings', () => {
    expect(num('3975.4800')).toBe('3 975,48');
    expect(num('144')).toBe('144');
    expect(num('10.0')).toBe('10');
  });
  it('formats unit prices with at least two decimals', () => {
    expect(price('312.5')).toBe('312,50');
    expect(price('8.6')).toBe('8,60');
    expect(price('65.77')).toBe('65,77');
    expect(price('8.625')).toBe('8,625');
    expect(price('9')).toBe('9,00');
  });
  it('usage floors each reading before subtracting', () => {
    expect(usage('3975.48', '3831')).toBe('144');
    expect(usage('3975.99', '3831.99')).toBe('144');
  });
  it('validates typed numbers', () => {
    expect(isValidNumber('761,5')).toBe(true);
    expect(isValidNumber('761.5')).toBe(true);
    expect(isValidNumber('1e5')).toBe(false);
    expect(isValidNumber('-3')).toBe(false);
    expect(isValidNumber('')).toBe(false);
  });
  it('names months and dates in Russian', () => {
    expect(monthName('2026-10')).toBe('Октябрь 2026');
    expect(dateLong('2026-10-16')).toBe('16 октября 2026');
    expect(monthFrom('2026-10')).toBe('октября 2026');
    expect(addMonth('2026-12', 1)).toBe('2027-01');
    expect(addMonth('2026-01', -1)).toBe('2025-12');
  });
});
