"""Entry point: python -m app"""
import sys
import os

# Ensure the project root is on the path
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from app.main import main

if __name__ == '__main__':
    main()
