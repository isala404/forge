// Re-export the client-preset fragment helpers under names that read as plain
// functions (they are pure casts, not React hooks) so conditional use does not
// trip the rules-of-hooks lint.
export {
  useFragment as readFragment,
  makeFragmentData,
  type FragmentType,
} from '../gql'
