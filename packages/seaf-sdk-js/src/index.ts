export const frameworkName = "Self-Evolving Application Framework";

export type PrivacyLevel = "public" | "aggregated" | "private" | "sensitive";

const privacyLevels = new Set<PrivacyLevel>([
  "public",
  "aggregated",
  "private",
  "sensitive",
]);

export interface SeafEvent {
  event_id: string;
  name: string;
  timestamp: string;
  source: string;
  privacy_level: PrivacyLevel;
  payload: Record<string, unknown>;
}

export interface EventOptions {
  eventId?: string;
  privacyLevel?: PrivacyLevel;
  source?: string;
  timestamp?: string;
}

export interface FeedbackInput {
  surface: string;
  sentiment: "confused" | "negative" | "neutral" | "positive";
  message: string;
}

export interface SeafTransport {
  send(event: SeafEvent): Promise<void>;
}

export interface SeafClientOptions {
  source: string;
  endpoint?: string;
  transport?: SeafTransport;
  clock?: () => Date;
  idGenerator?: () => string;
}

export interface SeafClient {
  event(
    name: string,
    payload?: Record<string, unknown>,
    options?: EventOptions,
  ): Promise<SeafEvent>;
  metric(
    name: string,
    value: number,
    options?: EventOptions,
  ): Promise<SeafEvent>;
  feedback(input: FeedbackInput, options?: EventOptions): Promise<SeafEvent>;
}

export function createSeafClient(options: SeafClientOptions): SeafClient {
  const transport =
    options.transport ??
    createHttpTransport(options.endpoint ?? "http://127.0.0.1:7373");
  const clock = options.clock ?? (() => new Date());
  const idGenerator = options.idGenerator ?? defaultIdGenerator;

  async function emit(
    name: string,
    payload: Record<string, unknown> = {},
    eventOptions: EventOptions = {},
  ): Promise<SeafEvent> {
    const event: SeafEvent = {
      event_id: eventOptions.eventId ?? idGenerator(),
      name,
      timestamp: eventOptions.timestamp ?? clock().toISOString(),
      source: eventOptions.source ?? options.source,
      privacy_level: eventOptions.privacyLevel ?? "aggregated",
      payload,
    };

    validateEvent(event);
    await transport.send(event);
    return event;
  }

  return {
    event: emit,
    metric(name, value, eventOptions) {
      return emit(name, { value }, eventOptions);
    },
    feedback(input, eventOptions) {
      const privacyLevel =
        eventOptions?.privacyLevel === "sensitive" ? "sensitive" : "private";
      return emit(
        "feedback.submitted",
        {
          surface: input.surface,
          sentiment: input.sentiment,
          message: input.message,
        },
        { ...eventOptions, privacyLevel },
      );
    },
  };
}

export function createHttpTransport(endpoint: string): SeafTransport {
  const base = endpoint.replace(/\/+$/, "");

  return {
    async send(event) {
      const response = await fetch(`${base}/v1/events`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify(event),
      });

      if (!response.ok) {
        throw new Error(`SEAF runtime rejected event with ${response.status}`);
      }
    },
  };
}

export function createMemoryTransport(): SeafTransport & {
  events: SeafEvent[];
} {
  return {
    events: [],
    async send(event) {
      this.events.push(event);
    },
  };
}

function validateEvent(event: SeafEvent): void {
  if (!event.event_id.trim()) {
    throw new Error("SEAF event_id must not be empty");
  }
  if (!event.name.trim()) {
    throw new Error("SEAF event name must not be empty");
  }
  if (!event.timestamp.trim()) {
    throw new Error("SEAF event timestamp must not be empty");
  }
  if (!event.source.trim()) {
    throw new Error("SEAF event source must not be empty");
  }
  if (!privacyLevels.has(event.privacy_level)) {
    throw new Error("SEAF event privacy_level is invalid");
  }
  if (!isPlainObject(event.payload)) {
    throw new Error("SEAF event payload must be an object");
  }
}

function defaultIdGenerator(): string {
  if (typeof crypto !== "undefined" && "randomUUID" in crypto) {
    return crypto.randomUUID();
  }

  return `evt_${Date.now()}_${Math.random().toString(16).slice(2)}`;
}

function isPlainObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
