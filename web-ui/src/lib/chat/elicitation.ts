export type ElicitationAction = "accept" | "decline" | "cancel";
export interface ElicitationField {
  name: string;
  title: string;
  description: string;
  kind: string;
  required: boolean;
  options: { value: string; label: string }[];
  fields: ElicitationField[];
  default: unknown;
  minimum: number | null;
  maximum: number | null;
  min_length: number | null;
  max_length: number | null;
  min_items: number | null;
  max_items: number | null;
  format: string | null;
}
export interface PendingElicitation {
  requestId: string;
  server: string;
  message: string;
  elicitation: {
    mode: string;
    fields: ElicitationField[];
    url: string | null;
    unsupported: string | null;
  };
}

/** A form's missing value is distinct from false, zero, and an empty list. */
export function formValues(fields: ElicitationField[], inputs: Record<string, unknown>): Record<string, unknown> {
  const result: Record<string, unknown> = Object.create(null);
  for (const field of fields) {
    const value = Object.hasOwn(inputs, field.name) ? inputs[field.name] : field.default;
    if (field.kind === "object") {
      if (!field.required && (value === null || value === undefined)) continue;
      const nested = formValues(field.fields, value && typeof value === "object" ? value as Record<string, unknown> : Object.create(null));
      if (field.required || Object.keys(nested).length) result[field.name] = nested;
      continue;
    }
    if (value === undefined || value === null || (value === "" && !field.required)) continue;
    if (field.kind === "number" || field.kind === "integer") {
      if (typeof value !== "number" && (typeof value !== "string" || value.trim() === "")) continue;
      result[field.name] = Number(value);
    } else result[field.name] = value;
  }
  return result;
}

export function formErrors(fields: ElicitationField[], values: Record<string, unknown>): Record<string, string> {
  const errors: Record<string, string> = Object.create(null);
  for (const field of fields) {
    const value = values[field.name];
    if (!Object.hasOwn(values, field.name)) {
      if (field.required) errors[field.name] = "This field is required.";
      continue;
    }
    if (field.kind === "object") {
      if (!value || typeof value !== "object" || Array.isArray(value)) errors[field.name] = "Enter the requested fields.";
      else {
        for (const [key, error] of Object.entries(formErrors(field.fields, value as Record<string, unknown>))) errors[`${field.name}.${key}`] = error;
      }
    } else if (field.kind === "number" || field.kind === "integer") {
      if (typeof value !== "number" || !Number.isFinite(value)) errors[field.name] = "Enter a number.";
      else if (field.kind === "integer" && !Number.isSafeInteger(value)) errors[field.name] = "Enter a whole number between -9007199254740991 and 9007199254740991.";
      else if ((field.minimum !== null && value < field.minimum) || (field.maximum !== null && value > field.maximum)) errors[field.name] = "Number is outside the allowed range.";
    } else if (field.kind === "boolean") {
      if (typeof value !== "boolean") errors[field.name] = "Choose yes or no.";
    } else if (field.kind === "array") {
      if (!Array.isArray(value) || value.some((v) => !field.options.some((option) => option.value === v)) || new Set(value).size !== value.length) errors[field.name] = "Choose distinct values from the list.";
      else if ((field.min_items !== null && value.length < field.min_items) || (field.max_items !== null && value.length > field.max_items)) errors[field.name] = "Choose the allowed number of values.";
    } else if (typeof value !== "string") errors[field.name] = "Enter text.";
    else if ((field.min_length !== null && [...value].length < field.min_length) || (field.max_length !== null && [...value].length > field.max_length)) errors[field.name] = "Text length is outside the allowed range.";
    else if (field.options.length && !field.options.some((option) => option.value === value)) errors[field.name] = "Choose a value from the list.";
  }
  return errors;
}

export function browserRequestUrl(value: string | null): URL | null {
  if (value === null || value.length > 8192 || /[\s\u0000-\u001f\u007f]/.test(value)) return null;
  try {
    const url = new URL(value);
    return ["http:", "https:"].includes(url.protocol) && !url.username && !url.password ? url : null;
  } catch { return null; }
}

export function loopbackUrl(url: URL | null): boolean {
  return url !== null && (url.hostname === "localhost" || url.hostname === "[::1]" || /^127\./.test(url.hostname));
}
