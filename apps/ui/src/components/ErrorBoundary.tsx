import { Component, type ErrorInfo, type ReactNode } from 'react';

/**
 * Keeps a crash in one view (for example the terminal renderer) from
 * unmounting the whole app: the fallback replaces only the failed subtree,
 * so navigation and the composer stay usable.
 */
export class ErrorBoundary extends Component<
  {
    fallback: (error: Error, reset: () => void) => ReactNode;
    /** Changing this key clears the error (for example a new session id). */
    resetKey?: unknown;
    children: ReactNode;
  },
  { error: Error | null; key: unknown }
> {
  state = { error: null as Error | null, key: this.props.resetKey };

  static getDerivedStateFromError(error: Error) {
    return { error };
  }

  static getDerivedStateFromProps(
    props: { resetKey?: unknown },
    state: { error: Error | null; key: unknown },
  ) {
    return props.resetKey !== state.key ? { error: null, key: props.resetKey } : null;
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    console.error('view crashed', error, info.componentStack);
  }

  reset = () => this.setState({ error: null });

  render() {
    return this.state.error
      ? this.props.fallback(this.state.error, this.reset)
      : this.props.children;
  }
}
