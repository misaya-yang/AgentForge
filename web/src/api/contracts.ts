import type { ApiOperations, ApiRequest, Operation } from "../../../sdk/typescript/generated";
export type { ApiRequest, ApiResponse, ApiEvent, Operation, UnaryOperation, StreamOperation } from "../../../sdk/typescript/generated";
export type RequestBody<K extends Operation> = ApiOperations[K]["body"];
export type RequestQuery<K extends Operation> = ApiRequest<K> extends { query?: infer Q } ? NonNullable<Q> : never;
/** UI workflows may require explicit values even when the server has defaults. */
export type Defined<T, K extends keyof T> = T & Required<Pick<T, K>>;
