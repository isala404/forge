import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import { Provider as UrqlProvider } from 'urql'
import { client } from './lib/urql'
import { App } from './App'
import './fonts.css'
import './index.css'
import './app.css'

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <UrqlProvider value={client}>
      <App />
    </UrqlProvider>
  </StrictMode>,
)
