export interface KVBackend {
  read(key: string): string | undefined;
  write(key: string, value: string): void;
  remove(key: string): void;
}

export type Unsubscribe = () => void;
export type Listener = () => void;

export interface KVSource<Value = unknown> {
  get(key: string): Value | undefined;
  set(key: string, value: Value): void;
  delete(key: string): void;
  subscribe(key: string, listener: Listener): Unsubscribe;
}

export interface KVBackends {
  web?: KVBackend;
  native?: KVBackend;
}

export type KVBackendOption = KVBackend | KVBackends;

export interface KVOptions<Value = unknown> {
  name: string;
  backend?: KVBackendOption;
  validate?: (value: unknown) => value is Value;
}
