/**
 * Dragonfly Gamepad Support
 * Provides Xbox, PlayStation, and other gamepad controller support
 * with a No Man's Sky-inspired cursor that snaps to UI elements
 */

class GamepadController {
    constructor() {
        // Controller state
        this.gamepads = [];
        this.gamepadConnected = false;
        this.activeElement = null;
        this.focusableElements = [];
        this.currentElementIndex = 0;
        this.gamepadCursorVisible = false;
        this.buttonStates = {};
        this.analogMoved = false;
        this.gamepadPollingInterval = null;
        
        // Initialize
        this.init();
    }
    
    init() {
        // Setup gamepad event listeners
        window.addEventListener('gamepadconnected', this.handleGamepadConnected.bind(this));
        window.addEventListener('gamepaddisconnected', this.handleGamepadDisconnected.bind(this));
        
        // Initialize focusable elements on page load
        document.addEventListener('DOMContentLoaded', () => {
            this.updateFocusableElements();
        });
    }
    
    handleGamepadConnected(e) {
        console.log('Gamepad connected:', e.gamepad.id);
        this.gamepads[e.gamepad.index] = e.gamepad;
        this.gamepadConnected = true;
        this.showGamepadUI();
        this.startGamepadPolling();
    }
    
    handleGamepadDisconnected(e) {
        console.log('Gamepad disconnected:', e.gamepad.id);
        delete this.gamepads[e.gamepad.index];
        this.gamepadConnected = Object.keys(this.gamepads).length > 0;
        
        if (!this.gamepadConnected) {
            this.hideGamepadUI();
            this.stopGamepadPolling();
        }
    }
    
    showGamepadUI() {
        this.gamepadCursorVisible = true;
        
        // Add styles if not already present
        if (!document.getElementById('gamepad-styles')) {
            const styleEl = document.createElement('style');
            styleEl.id = 'gamepad-styles';
            styleEl.textContent = `
                .gamepad-focus {
                    outline: 3px solid rgba(99, 102, 241, 0.8) !important;
                    outline-offset: 4px !important;
                    position: relative;
                    z-index: 40;
                    box-shadow: 0 0 15px rgba(99, 102, 241, 0.5);
                    animation: pulse 2s cubic-bezier(0.4, 0, 0.6, 1) infinite;
                }
                
                @keyframes pulse {
                    0%, 100% { opacity: 1; }
                    50% { opacity: 0.7; }
                }
                
                #gamepad-cursor {
                    pointer-events: none;
                    position: fixed;
                    width: 20px;
                    height: 20px;
                    z-index: 9999;
                    transition: transform 0.1s ease-out;
                }
                
                #gamepad-hint {
                    transition: opacity 0.5s ease;
                }
            `;
            document.head.appendChild(styleEl);
        }
        
        // Focus the first element
        this.focusElementAtIndex(0);
        
        // Show gamepad controls hint
        const hint = document.createElement('div');
        hint.id = 'gamepad-hint';
        hint.className = 'fixed bottom-4 left-4 bg-black bg-opacity-70 text-white p-3 rounded-lg text-sm';
        hint.innerHTML = `
          <div class="flex items-center space-x-2">
            <span>🎮</span>
            <span>Use D-pad/sticks to navigate, A/X to select, B/Circle to back</span>
          </div>
        `;
        document.body.appendChild(hint);
        setTimeout(() => {
          if (hint) hint.classList.add('opacity-50');
        }, 5000);
    }
    
    hideGamepadUI() {
        this.gamepadCursorVisible = false;
        const cursor = document.getElementById('gamepad-cursor');
        if (cursor) cursor.remove();
        
        const hint = document.getElementById('gamepad-hint');
        if (hint) hint.remove();
        
        this.clearFocusStyles();
    }
    
    updateFocusableElements() {
        // Get all focusable elements (links, buttons, form elements)
        this.focusableElements = Array.from(document.querySelectorAll('a, button, select, input, textarea, [tabindex]:not([tabindex="-1"])'))
          .filter(el => {
            // Ensure element is visible and not disabled
            const style = window.getComputedStyle(el);
            return style.display !== 'none' && 
                   style.visibility !== 'hidden' && 
                   !el.hasAttribute('disabled');
          });
        
        // Include the mode cards in the welcome page specifically
        const modeCards = document.querySelectorAll('.grid-cols-1.md\\:grid-cols-3 > div');
        if (modeCards.length) {
            modeCards.forEach(card => {
                if (!this.focusableElements.includes(card)) {
                    this.focusableElements.push(card);
                }
            });
        }
    }
    
    focusElementAtIndex(index) {
        if (this.focusableElements.length === 0) return;
        
        this.updateFocusableElements(); // Refresh elements to ensure we have the latest
        
        if (index < 0) index = 0;
        if (index >= this.focusableElements.length) index = this.focusableElements.length - 1;
        
        this.currentElementIndex = index;
        this.activeElement = this.focusableElements[index];
        
        // Scroll element into view if needed
        this.activeElement.scrollIntoView({ behavior: 'smooth', block: 'center' });
        
        // Clear any existing focus styles first
        this.clearFocusStyles();
        
        // Add focus styles
        this.activeElement.classList.add('gamepad-focus');
        
        // Position the cursor near the element
        const cursor = document.getElementById('gamepad-cursor');
        if (cursor && this.activeElement) {
            const rect = this.activeElement.getBoundingClientRect();
            const left = rect.left + rect.width / 2 - 10;
            const top = rect.top - 20;
            cursor.style.transform = `translate(${left}px, ${top}px)`;
        }
    }
    
    startGamepadPolling() {
        if (this.gamepadPollingInterval) return;
        
        this.gamepadPollingInterval = setInterval(() => {
            // Get all connected gamepads
            const gamepads = navigator.getGamepads();
            
            for (const gamepad of gamepads) {
                if (!gamepad) continue;
                
                // Handle buttons (check for pressed state changes)
                this.handleGamepadInput(gamepad);
            }
        }, 100); // Poll at 10Hz
    }
    
    stopGamepadPolling() {
        if (this.gamepadPollingInterval) {
            clearInterval(this.gamepadPollingInterval);
            this.gamepadPollingInterval = null;
        }
    }
    
    handleGamepadInput(gamepad) {
        // Process buttons
        // A button (Xbox) or X button (PlayStation)
        if (gamepad.buttons[0].pressed && !this.buttonStates?.a) {
            this.buttonStates = {...this.buttonStates, a: true};
            
            // Simulate a click on the active element
            if (this.activeElement) {
                this.activeElement.click();
            }
        } else if (!gamepad.buttons[0].pressed && this.buttonStates?.a) {
            this.buttonStates = {...this.buttonStates, a: false};
        }
        
        // B button (Xbox) or Circle button (PlayStation) - go back
        if (gamepad.buttons[1].pressed && !this.buttonStates?.b) {
            this.buttonStates = {...this.buttonStates, b: true};
            window.history.back();
        } else if (!gamepad.buttons[1].pressed && this.buttonStates?.b) {
            this.buttonStates = {...this.buttonStates, b: false};
        }
        
        // Process D-pad (digital)
        // D-pad Up
        if (gamepad.buttons[12]?.pressed && !this.buttonStates?.up) {
            this.buttonStates = {...this.buttonStates, up: true};
            this.navigateUp();
        } else if (!gamepad.buttons[12]?.pressed && this.buttonStates?.up) {
            this.buttonStates = {...this.buttonStates, up: false};
        }
        
        // D-pad Down
        if (gamepad.buttons[13]?.pressed && !this.buttonStates?.down) {
            this.buttonStates = {...this.buttonStates, down: true};
            this.navigateDown();
        } else if (!gamepad.buttons[13]?.pressed && this.buttonStates?.down) {
            this.buttonStates = {...this.buttonStates, down: false};
        }
        
        // D-pad Left
        if (gamepad.buttons[14]?.pressed && !this.buttonStates?.left) {
            this.buttonStates = {...this.buttonStates, left: true};
            this.navigateLeft();
        } else if (!gamepad.buttons[14]?.pressed && this.buttonStates?.left) {
            this.buttonStates = {...this.buttonStates, left: false};
        }
        
        // D-pad Right
        if (gamepad.buttons[15]?.pressed && !this.buttonStates?.right) {
            this.buttonStates = {...this.buttonStates, right: true};
            this.navigateRight();
        } else if (!gamepad.buttons[15]?.pressed && this.buttonStates?.right) {
            this.buttonStates = {...this.buttonStates, right: false};
        }
        
        // Process analog sticks
        // Left analog stick
        const leftX = gamepad.axes[0];
        const leftY = gamepad.axes[1];
        
        if (Math.abs(leftX) > 0.5 || Math.abs(leftY) > 0.5) {
            // Only trigger once per stick movement
            if (!this.analogMoved) {
                this.analogMoved = true;
                
                if (leftX < -0.5) this.navigateLeft();
                else if (leftX > 0.5) this.navigateRight();
                
                if (leftY < -0.5) this.navigateUp();
                else if (leftY > 0.5) this.navigateDown();
            }
        } else {
            this.analogMoved = false;
        }
    }
    
    navigateUp() {
        const prevIndex = this.findAdjacentElement('up');
        if (prevIndex !== -1 && prevIndex !== this.currentElementIndex) {
            this.focusElementAtIndex(prevIndex);
        }
    }
    
    navigateDown() {
        const nextIndex = this.findAdjacentElement('down');
        if (nextIndex !== -1 && nextIndex !== this.currentElementIndex) {
            this.focusElementAtIndex(nextIndex);
        }
    }
    
    navigateLeft() {
        const leftIndex = this.findAdjacentElement('left');
        if (leftIndex !== -1 && leftIndex !== this.currentElementIndex) {
            this.focusElementAtIndex(leftIndex);
        }
    }
    
    navigateRight() {
        const rightIndex = this.findAdjacentElement('right');
        if (rightIndex !== -1 && rightIndex !== this.currentElementIndex) {
            this.focusElementAtIndex(rightIndex);
        }
    }
    
    findAdjacentElement(direction) {
        if (!this.activeElement || this.focusableElements.length === 0) return -1;
        
        const currentRect = this.activeElement.getBoundingClientRect();
        const currentCenterX = currentRect.left + currentRect.width / 2;
        const currentCenterY = currentRect.top + currentRect.height / 2;
        
        let bestIndex = -1;
        let bestDistance = Number.POSITIVE_INFINITY;
        
        // Check all focusable elements
        this.focusableElements.forEach((element, index) => {
            if (element === this.activeElement) return;
            
            const rect = element.getBoundingClientRect();
            const centerX = rect.left + rect.width / 2;
            const centerY = rect.top + rect.height / 2;
            
            // Check if element is in the right direction
            let inRightDirection = false;
            switch (direction) {
                case 'up':
                    inRightDirection = centerY < currentCenterY;
                    break;
                case 'down':
                    inRightDirection = centerY > currentCenterY;
                    break;
                case 'left':
                    inRightDirection = centerX < currentCenterX;
                    break;
                case 'right':
                    inRightDirection = centerX > currentCenterX;
                    break;
            }
            
            if (inRightDirection) {
                // Calculate distance based on direction priority
                let distance;
                
                if (direction === 'up' || direction === 'down') {
                    // For up/down prioritize vertical distance
                    distance = Math.abs(centerY - currentCenterY) * 3 + Math.abs(centerX - currentCenterX);
                } else {
                    // For left/right prioritize horizontal distance
                    distance = Math.abs(centerX - currentCenterX) * 3 + Math.abs(centerY - currentCenterY);
                }
                
                if (distance < bestDistance) {
                    bestDistance = distance;
                    bestIndex = index;
                }
            }
        });
        
        return bestIndex;
    }
    
    clearFocusStyles() {
        document.querySelectorAll('.gamepad-focus').forEach(el => {
            el.classList.remove('gamepad-focus');
        });
    }
}

// Initialize the gamepad controller when the script loads
document.addEventListener('DOMContentLoaded', () => {
    window.gamepadController = new GamepadController();
}); 