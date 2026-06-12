// Globals the Grayjay V8 host provides but @types/grayjay-source doesn't declare.
// `console` is available at runtime (the plugin's load marker logs through it).
declare const console: {
	log: (...args: unknown[]) => void;
	warn: (...args: unknown[]) => void;
	error: (...args: unknown[]) => void;
};
